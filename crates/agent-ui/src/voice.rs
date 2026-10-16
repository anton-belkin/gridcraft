//! Voice questions: hold ⌥ (Alt) or click the microphone on the Agent tab, speak, click the cells
//! you mean while speaking; release, and the question goes on in the background.
//!
//! The question is put on the cells at once ("transcribing" flag, anchors on every cell pointed
//! at, so they move with edits); [`craft_voice`] transcribes it on a worker thread and the text,
//! with a marker `⟦n⟧` where each cell was pointed at, goes to the agent's queue (`agent.spoken`).
//! A failed transcription is a flag with Retry and Dismiss, never a dialog. Audio stays in memory
//! until its text is there (or the reader dismisses it), and is never written anywhere.
//!
//! The model is the reader's choice on the Agent tab (off until they pick one), an app setting
//! (`voice.json` beside the agent keys), never the workbook's.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use craft_voice::{Audio, Event, Model, Pointer, Recorder, Transcriber};
use egui::{Align2, Color32, Rect, Stroke, pos2, vec2};
use gridcraft_agent::anchor;
use gridcraft_engine::Session;
use gridcraft_engine::core::RangeRef;
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::theme::{self, Tokens};
use serde_json::{Value as Json, json};

/// How long ⌥ is held alone before recording starts (a tap or a shortcut is not a question).
const HOLD: f64 = 0.2;
/// Shorter recordings with nothing pointed at are taken for an accidental press.
const MIN_SECONDS: f32 = 0.4;
/// A loaded model is let go after this long without a question (it holds 0.6–1.3 GB).
const IDLE_UNLOAD: f64 = 15.0 * 60.0;

/// The key, as people see it.
pub(crate) const KEY: &str = if cfg!(target_os = "macos") { "⌥ Option" } else { "Alt" };

/// Where the model stands, for the ribbon.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Status {
    /// No model chosen.
    #[default]
    Off,
    /// Chosen, downloaded, not loaded (loads with the first question).
    Idle,
    Downloading(u64, u64),
    Loading,
    Ready,
    Failed(String),
}

impl Status {
    fn id(&self) -> &'static str {
        match self {
            Status::Off => "off",
            Status::Idle => "idle",
            Status::Downloading(..) => "downloading",
            Status::Loading => "loading",
            Status::Ready => "ready",
            Status::Failed(_) => "failed",
        }
    }
}

/// One cell pointed at: the place (`Sheet1!C16`) in the document being asked about.
type Anchor = String;

struct Recording {
    /// None in a preview (offscreen snapshots): no microphone.
    recorder: Option<Recorder>,
    preview_secs: f32,
    doc: u64,
    /// The selection when it started: the question's main place.
    anchor: Anchor,
    pointers: Vec<Pointer<Anchor>>,
    /// The selection last seen, and when a click in the grid began (seconds into the recording).
    last: (usize, RangeRef),
    press: Option<f32>,
    by_key: bool,
}

/// A question waiting for its text.
struct Job {
    doc: u64,
    question: String,
    pointers: Vec<Pointer<Anchor>>,
    /// Kept for a retry until the text is there.
    audio: Audio,
    /// The id of its transcription now.
    ticket: u64,
    failed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Key {
    #[default]
    Idle,
    Armed(f64),
    /// ⌥ went into a shortcut: wait for it to be released.
    Chord,
    Recording,
}

#[derive(Default)]
pub(crate) struct Voice {
    choice: Option<Model>,
    settings_read: bool,
    transcriber: Option<Transcriber>,
    status: Status,
    rec: Option<Recording>,
    jobs: Vec<Job>,
    next_ticket: u64,
    key: Key,
    last_used: f64,
}

/// A model chosen through `agent.voiceModel` (control channel, MCP), taken by the UI next frame.
static REQUEST: Mutex<Option<Option<Model>>> = Mutex::new(None);
/// What the ribbon shows, for `agent.voiceStatus`.
static SNAPSHOT: Mutex<Option<Json>> = Mutex::new(None);
/// The settings file, when a test moves it.
static SETTINGS: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What an offscreen snapshot shows instead of a live microphone and download.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct Preview {
    /// Recording for this many seconds, these places pointed at (`Sheet1!C5`).
    pub recording: Option<(f32, Vec<String>)>,
    /// A download this far along (bytes, total).
    pub downloading: Option<(u64, u64)>,
}

static PREVIEW: Mutex<Option<Preview>> = Mutex::new(None);

/// Shows a recording or a download without a microphone or a network (snapshots).
#[doc(hidden)]
pub fn preview(p: Preview) {
    if let Ok(mut g) = PREVIEW.lock() {
        *g = Some(p);
    }
}

/// Moves the settings file (tests).
pub fn set_settings_path(p: Option<PathBuf>) {
    if let Ok(mut g) = SETTINGS.lock() {
        *g = p;
    }
}

fn settings_path() -> Option<PathBuf> {
    if let Some(p) = SETTINGS.lock().ok().and_then(|g| g.clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("GRIDCRAFT_VOICE_SETTINGS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    gridcraft_agent::safe::key_dir().and_then(|d| d.parent().map(|p| p.join("voice.json")))
}

fn read_choice() -> Option<Model> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    let v: Json = serde_json::from_str(&text).ok()?;
    Model::from_id(v.get("model")?.as_str()?)
}

fn write_choice(m: Option<Model>) -> Result<(), String> {
    let Some(p) = settings_path() else { return Err("no settings folder".into()) };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, json!({"model": m.map_or("off", Model::id)}).to_string()).map_err(|e| e.to_string())
}

fn model_dir() -> Option<PathBuf> {
    craft_voice::default_dir()
}

// ---------------------------------------------------------------- commands

pub(crate) static SPECS: &[gridcraft_engine::cmd::CommandSpec] = &[
    gridcraft_engine::cmd::CommandSpec {
        id: "agent.voiceModel",
        label: "Voice Questions Model",
        menu: &[],
        shortcut: None,
        params: "{model: off|small|turbo} — the speech model for voice questions (an app setting; downloaded on first use)",
        enabled: gridcraft_engine::cmd::always,
        run: voice_model,
        journal: false,
        undoable: false,
    },
    gridcraft_engine::cmd::CommandSpec {
        id: "agent.voiceStatus",
        label: "Voice Questions Status",
        menu: &[],
        shortcut: None,
        params: "{} → {model, status, done?, total?, error?, recording, transcribing}",
        enabled: gridcraft_engine::cmd::always,
        run: voice_status,
        journal: false,
        undoable: false,
    },
];

fn voice_model(_s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    let bad = |m: &str| gridcraft_engine::EngineError::BadParams { cmd: "agent.voiceModel".into(), msg: m.into() };
    let id = p.get("model").and_then(Json::as_str).ok_or_else(|| bad("missing `model` (off, small or turbo)"))?;
    let m = match id {
        "off" => None,
        id => Some(Model::from_id(id).ok_or_else(|| bad("`model` is off, small or turbo"))?),
    };
    write_choice(m).map_err(|e| gridcraft_engine::EngineError::Other(format!("couldn't keep the setting: {e}")))?;
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(m);
    }
    Ok(json!({"model": m.map_or("off", Model::id)}))
}

fn voice_status(_s: &mut Session, _p: &Json) -> gridcraft_engine::Result<Json> {
    Ok(SNAPSHOT.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| json!({"model": "off", "status": "off"})))
}

// ---------------------------------------------------------------- the frame

impl Voice {
    pub(crate) fn choice(&self) -> Option<Model> {
        self.choice
    }

    pub(crate) fn status(&self) -> &Status {
        &self.status
    }

    pub(crate) fn recording(&self) -> bool {
        self.rec.is_some()
    }

    /// Seconds recorded and the input level, while recording.
    pub(crate) fn meter(&self) -> Option<(f32, f32)> {
        self.rec.as_ref().map(|r| r.recorder.as_ref().map_or((r.preview_secs, 0.35), |m| (m.position(), m.level())))
    }

    /// The cells pointed at so far, numbered (while recording).
    pub(crate) fn pointed(&self) -> Vec<(u32, String)> {
        let Some(r) = &self.rec else { return vec![] };
        numbered(&r.pointers)
    }

    /// The reader picked a model (or off) on the ribbon.
    pub(crate) fn choose(&mut self, app: &mut SheetApp, ctx: &egui::Context, m: Option<Model>) {
        if let Err(e) = write_choice(m) {
            app.toast = Some((format!("Voice: couldn't keep the setting ({e})"), gridcraft_ui_egui::now_ms()));
        }
        self.apply(app, ctx, m);
    }

    fn apply(&mut self, app: &mut SheetApp, ctx: &egui::Context, m: Option<Model>) {
        self.settings_read = true;
        if self.choice == m {
            return;
        }
        self.choice = m;
        self.transcriber = None;
        self.status = match m {
            None => Status::Off,
            Some(_) => Status::Idle,
        };
        match m {
            // Downloaded on first use: picking a model that isn't here downloads it now, with
            // its progress on the ribbon. One that is here loads with the first question.
            Some(m) if !model_dir().is_some_and(|d| m.is_ready(&d)) => self.start(ctx),
            _ => {}
        }
        // Questions waiting for text go to the new model (or fail when voice is off).
        let tickets: Vec<usize> = (0..self.jobs.len()).filter(|&i| self.jobs.get(i).is_some_and(|j| !j.failed)).collect();
        for i in tickets {
            self.resend(app, ctx, i);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (Some(m), Some(dir)) = (self.choice, model_dir()) else { return };
        if self.transcriber.is_some() {
            return;
        }
        let c = ctx.clone();
        let languages = std::env::var("GRIDCRAFT_VOICE_LANGUAGES")
            .map(|v| v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default();
        self.transcriber = Some(Transcriber::start(m, dir, craft_voice::Options { languages }, move || c.request_repaint()));
        self.status = if m.is_ready(&model_dir().unwrap_or_default()) { Status::Loading } else { Status::Downloading(0, m.download_size()) };
    }

    /// Every frame: the key, the pointing, the transcriber's news.
    pub(crate) fn frame(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        if !self.settings_read {
            self.settings_read = true;
            let m = read_choice();
            self.apply(app, ctx, m);
        }
        if let Some(m) = REQUEST.lock().ok().and_then(|mut r| r.take()) {
            self.apply(app, ctx, m);
        }
        if let Some(p) = PREVIEW.lock().ok().and_then(|mut g| g.take()) {
            self.show_preview(app, p);
        }
        let now = ctx.input(|i| i.time);
        self.key(app, ctx, now);
        self.pointing(app, ctx);
        self.news(app, ctx, now);
        if self.jobs.is_empty() && self.rec.is_none() && self.transcriber.is_some() && now - self.last_used > IDLE_UNLOAD {
            // Let the model go; the next question loads it again.
            self.transcriber = None;
            self.status = if self.choice.is_some() { Status::Idle } else { Status::Off };
        }
        if let Ok(mut g) = SNAPSHOT.lock() {
            let mut v = json!({
                "model": self.choice.map_or("off", Model::id),
                "status": self.status.id(),
                "recording": self.rec.is_some(),
                "transcribing": self.jobs.iter().filter(|j| !j.failed).count(),
                "failed": self.jobs.iter().filter(|j| j.failed).count(),
            });
            match &self.status {
                Status::Downloading(done, total) => {
                    v["done"] = json!(done);
                    v["total"] = json!(total);
                }
                Status::Failed(e) => v["error"] = json!(e),
                _ => {}
            }
            *g = Some(v);
        }
        if self.rec.is_some() {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    /// Push to talk: ⌥ held alone starts recording, its release sends; Esc cancels.
    fn key(&mut self, app: &mut SheetApp, ctx: &egui::Context, now: f64) {
        let (mods, keys, esc) = ctx.input(|i| {
            let keys = i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)));
            let esc = i.key_pressed(egui::Key::Escape);
            (i.modifiers, keys, esc)
        });
        let alone = mods.alt && !mods.shift && !mods.command && !mods.ctrl && !mods.mac_cmd;
        if esc && self.rec.is_some() {
            self.cancel();
            self.key = if mods.alt { Key::Chord } else { Key::Idle };
            return;
        }
        let typing = app.editor.is_some() || ctx.egui_wants_keyboard_input();
        self.key = match self.key {
            Key::Idle if alone && !typing && self.choice.is_some() && self.rec.is_none() && app.session.active().is_some() => {
                ctx.request_repaint_after(Duration::from_secs_f64(HOLD + 0.02));
                Key::Armed(now)
            }
            Key::Idle => Key::Idle,
            Key::Armed(_) if !mods.alt => Key::Idle,
            Key::Armed(_) if keys || !alone => Key::Chord,
            Key::Armed(t) if now - t >= HOLD => {
                if self.begin(app, ctx, true) {
                    Key::Recording
                } else {
                    Key::Chord
                }
            }
            Key::Armed(t) => {
                ctx.request_repaint_after(Duration::from_secs_f64((HOLD - (now - t)).max(0.0) + 0.02));
                Key::Armed(t)
            }
            Key::Chord if mods.alt => Key::Chord,
            Key::Chord => Key::Idle,
            Key::Recording if self.rec.is_none() => {
                if mods.alt {
                    Key::Chord
                } else {
                    Key::Idle
                }
            }
            Key::Recording if !mods.alt => {
                if self.rec.as_ref().is_some_and(|r| r.by_key) {
                    self.finish(app, ctx);
                }
                Key::Idle
            }
            Key::Recording => Key::Recording,
        };
    }

    fn show_preview(&mut self, app: &SheetApp, p: Preview) {
        if let Some((done, total)) = p.downloading {
            self.status = Status::Downloading(done, total);
        }
        let (Some((secs, places)), Some(d)) = (p.recording, app.session.active()) else { return };
        let si = d.wb.active_sheet;
        let sel = d.selection.current();
        let name = d.wb.sheet(si).map(|s| s.name.clone()).unwrap_or_default();
        let n = places.len().max(1) as f32;
        self.rec = Some(Recording {
            recorder: None,
            preview_secs: secs,
            doc: d.uid,
            anchor: anchor::Place { sheet: si, sheet_name: name, range: sel }.a1(),
            pointers: places.into_iter().enumerate().map(|(i, a)| Pointer { at: secs * (i as f32 + 0.5) / n, anchor: a }).collect(),
            last: (si, sel),
            press: None,
            by_key: true,
        });
    }

    /// The microphone button: start, or stop and send.
    pub(crate) fn toggle(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        if self.rec.is_some() {
            self.finish(app, ctx);
        } else {
            self.begin(app, ctx, false);
        }
    }

    fn begin(&mut self, app: &mut SheetApp, ctx: &egui::Context, by_key: bool) -> bool {
        let Some(d) = app.session.active() else { return false };
        if self.choice.is_none() {
            app.toast = Some(("Voice questions: choose a speech model on the Agent tab first".into(), gridcraft_ui_egui::now_ms()));
            return false;
        }
        let si = d.wb.active_sheet;
        let sheet = d.wb.sheet(si).map(|s| s.name.clone()).unwrap_or_default();
        let sel = d.selection.current();
        let place = anchor::Place { sheet: si, sheet_name: sheet, range: sel };
        let doc = d.uid;
        match Recorder::start() {
            Ok(recorder) => {
                self.rec = Some(Recording {
                    recorder: Some(recorder),
                    preview_secs: 0.0,
                    doc,
                    anchor: place.a1(),
                    pointers: vec![],
                    last: (si, sel),
                    press: None,
                    by_key,
                });
                // Load the model while the reader speaks.
                self.start(ctx);
                self.last_used = ctx.input(|i| i.time);
                true
            }
            Err(e) => {
                app.toast = Some((format!("Voice: {e}"), gridcraft_ui_egui::now_ms()));
                false
            }
        }
    }

    fn cancel(&mut self) {
        // Dropping the recorder drops the audio.
        self.rec = None;
    }

    /// While recording: every click in the grid and every selection change is a pointer, at the
    /// moment it began.
    fn pointing(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        let Some(r) = self.rec.as_mut() else { return };
        let Some(d) = app.session.active() else { return };
        if d.uid != r.doc {
            return;
        }
        let at = r.recorder.as_ref().map_or(r.preview_secs, Recorder::position);
        let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        if pressed
            && let (Some(cells), Some(p)) = (app.grid.cells_rect, pos)
            && cells.contains(p)
        {
            r.press = Some(at);
        }
        let si = d.wb.active_sheet;
        let sel = d.selection.current();
        let here = || {
            let name = d.wb.sheet(si).map(|s| s.name.clone()).unwrap_or_default();
            anchor::Place { sheet: si, sheet_name: name, range: sel }.a1()
        };
        match r.press {
            // A click (or a drag) ended: what it selected, at the moment it began.
            Some(t) if !down => {
                r.pointers.push(Pointer { at: t, anchor: here() });
                r.press = None;
                r.last = (si, sel);
            }
            Some(_) => {}
            // The selection moved some other way (keys, the Name Box).
            None if r.last != (si, sel) => {
                r.pointers.push(Pointer { at, anchor: here() });
                r.last = (si, sel);
            }
            None => {}
        }
    }

    /// Key up: the question goes on the cells now, its text follows.
    fn finish(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        let Some(r) = self.rec.take() else { return };
        let (doc, main, pointers) = (r.doc, r.anchor, r.pointers);
        let Some(recorder) = r.recorder else { return };
        let audio = match recorder.stop() {
            Ok(a) => a,
            Err(e) => {
                app.toast = Some((format!("Voice: {e}"), gridcraft_ui_egui::now_ms()));
                return;
            }
        };
        if audio.seconds() < MIN_SECONDS && pointers.is_empty() {
            return;
        }
        self.submit(app, ctx, doc, main, pointers, audio);
    }

    /// A recording to ask about: on the cells now (transcribing), to the transcriber.
    fn submit(&mut self, app: &mut SheetApp, ctx: &egui::Context, doc: u64, main: Anchor, pointers: Vec<Pointer<Anchor>>, audio: Audio) {
        let refs: Vec<Json> = numbered(&pointers).into_iter().map(|(n, a)| json!({"n": n, "range": a})).collect();
        let params = json!({"range": main, "transcribing": true, "spoken": true, "refs": refs});
        let question = match on_doc(&mut app.session, doc, "agent.ask", params) {
            Ok(v) => v.get("id").and_then(Json::as_str).unwrap_or_default().to_string(),
            Err(e) => {
                app.toast = Some((format!("Voice: {e}"), gridcraft_ui_egui::now_ms()));
                return;
            }
        };
        self.jobs.push(Job { doc, question, pointers, audio, ticket: 0, failed: false });
        let i = self.jobs.len() - 1;
        self.resend(app, ctx, i);
    }

    /// (Re)sends a job's audio to the transcriber.
    fn resend(&mut self, app: &mut SheetApp, ctx: &egui::Context, i: usize) {
        self.start(ctx);
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        let Some(job) = self.jobs.get_mut(i) else { return };
        let sent = self.transcriber.as_ref().is_some_and(|t| t.transcribe(ticket, job.audio.clone()));
        job.ticket = ticket;
        job.failed = !sent;
        let (doc, q) = (job.doc, job.question.clone());
        let params =
            if sent { json!({"id": q}) } else { json!({"id": q, "error": "Voice questions are off: choose a speech model on the Agent tab"}) };
        let _ = on_doc(&mut app.session, doc, "agent.spoken", params);
        self.last_used = ctx.input(|i| i.time);
    }

    /// Retry or Dismiss on a failed spoken question.
    pub(crate) fn retry(&mut self, app: &mut SheetApp, ctx: &egui::Context, question: &str) {
        match self.jobs.iter().position(|j| j.question == question) {
            Some(i) => self.resend(app, ctx, i),
            // The audio is gone (the app was restarted): nothing to retry.
            None => app.toast = Some(("This recording is no longer here: ask again".into(), gridcraft_ui_egui::now_ms())),
        }
    }

    pub(crate) fn dismiss(&mut self, app: &mut SheetApp, question: &str) {
        let doc = self.jobs.iter().find(|j| j.question == question).map(|j| j.doc);
        self.jobs.retain(|j| j.question != question);
        let uid = doc.or_else(|| app.session.active().map(|d| d.uid)).unwrap_or_default();
        let _ = on_doc(&mut app.session, uid, "agent.removeQuestion", json!({"id": question}));
    }

    fn news(&mut self, app: &mut SheetApp, ctx: &egui::Context, now: f64) {
        let Some(t) = &self.transcriber else { return };
        for e in t.poll() {
            match e {
                Event::Downloading { done, total, .. } => self.status = Status::Downloading(done, total),
                Event::Loading { .. } => self.status = Status::Loading,
                Event::Ready { .. } => self.status = Status::Ready,
                Event::Failed { id: None, error } => self.status = Status::Failed(error),
                Event::Failed { id: Some(ticket), error } => {
                    if let Some(j) = self.jobs.iter_mut().find(|j| j.ticket == ticket) {
                        j.failed = true;
                        let (doc, q) = (j.doc, j.question.clone());
                        let _ = on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "error": error}));
                    }
                }
                Event::Text { id: ticket, transcript, .. } => {
                    self.last_used = now;
                    let Some(i) = self.jobs.iter().position(|j| j.ticket == ticket) else { continue };
                    let Some(j) = self.jobs.get_mut(i) else { continue };
                    let (doc, q) = (j.doc, j.question.clone());
                    if transcript.is_empty() {
                        j.failed = true;
                        let _ = on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "error": "Nothing was heard"}));
                        continue;
                    }
                    let text = craft_voice::splice(&transcript, &j.pointers).text;
                    match on_doc(&mut app.session, doc, "agent.spoken", json!({"id": q, "text": text})) {
                        // With the agent now; the audio goes.
                        Ok(_) => {
                            self.jobs.remove(i);
                        }
                        // The question was dismissed or its workbook closed.
                        Err(_) => {
                            self.jobs.remove(i);
                        }
                    }
                }
            }
        }
        // Questions whose workbook was closed: let their audio go.
        let open: Vec<u64> = app.session.documents().iter().map(|d| d.uid).collect();
        self.jobs.retain(|j| open.contains(&j.doc));
        let _ = ctx;
    }
}

/// The references in the order they were pointed at, the same place reusing its number: the
/// numbering `craft_voice::splice` gives the markers.
fn numbered(pointers: &[Pointer<Anchor>]) -> Vec<(u32, Anchor)> {
    let mut order: Vec<&Pointer<Anchor>> = pointers.iter().collect();
    order.sort_by(|a, b| a.at.total_cmp(&b.at));
    let mut out: Vec<(u32, Anchor)> = vec![];
    for p in order {
        if !out.iter().any(|(_, a)| *a == p.anchor) {
            let n = u32::try_from(out.len()).unwrap_or(u32::MAX).saturating_add(1);
            out.push((n, p.anchor.clone()));
        }
    }
    out
}

/// Runs a command on a given workbook (the one the question was asked in), whichever is in front.
pub(crate) fn on_doc(s: &mut Session, uid: u64, id: &str, params: Json) -> Result<Json, String> {
    let Some(i) = s.documents().iter().position(|d| d.uid == uid) else { return Err("the workbook was closed".into()) };
    let front = s.active_index();
    s.set_active(i);
    let r = s.execute(id, params).map_err(|e| e.to_string());
    s.set_active(front);
    r
}

// ---------------------------------------------------------------- drawing

/// While recording: a pill at the bottom of the sheet (time, level, how to send or cancel).
pub(crate) fn indicator(v: &Voice, app: &SheetApp, ctx: &egui::Context) {
    let (Some((secs, level)), Some(cells)) = (v.meter(), app.grid.cells_rect) else { return };
    let t = Tokens::get(ctx);
    let red = Color32::from_rgb(0xD9, 0x30, 0x25);
    let by_key = v.rec.as_ref().is_some_and(|r| r.by_key);
    let hint = if by_key { format!("release {KEY} to send · Esc cancels") } else { "click the microphone to send · Esc cancels".into() };
    let n = v.pointed().len();
    let text = if n > 0 {
        format!("Listening {secs:.0}s · {n} pointed · {hint}")
    } else {
        format!("Listening {secs:.0}s · click the cells you mean · {hint}")
    };
    let pos = pos2(cells.center().x, cells.bottom() - 18.0);
    egui::Area::new(egui::Id::new("agent_voice_indicator"))
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE.fill(t.window).stroke(Stroke::new(1.0, red)).corner_radius(14.0).inner_margin(egui::Margin::symmetric(10, 5)).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(64.0, 16.0), egui::Sense::hover());
                        let pulse = 0.6 + 0.4 * ((ui.input(|i| i.time) * 3.0).sin() * 0.5 + 0.5) as f32;
                        ui.painter().circle_filled(pos2(r.left() + 6.0, r.center().y), 5.0, red.gamma_multiply(pulse));
                        // A level meter: five bars.
                        let lv = (level * 8.0).clamp(0.0, 1.0);
                        for k in 0..5 {
                            let h = 3.0 + 10.0 * (lv * (1.0 - (k as f32 - 2.0).abs() * 0.18)).max(0.05);
                            let x = r.left() + 18.0 + k as f32 * 8.0;
                            let bar = Rect::from_center_size(pos2(x, r.center().y), vec2(4.0, h));
                            ui.painter().rect_filled(bar, 2.0, if lv > 0.05 { red } else { t.text_dim });
                        }
                        ui.label(egui::RichText::new(text).font(theme::ui_font(12.0)).color(t.text));
                    });
                },
            );
        });
}

/// While recording: a numbered bubble on each cell pointed at.
pub(crate) fn bubbles(v: &Voice, app: &SheetApp, painter: &egui::Painter, cells: &gridcraft_ui_egui::ext::CellGeometry) {
    let Some(d) = app.session.active() else { return };
    if v.rec.as_ref().is_none_or(|r| r.doc != d.uid) {
        return;
    }
    let red = Color32::from_rgb(0xD9, 0x30, 0x25);
    for (n, a) in v.pointed() {
        let Some(pl) = anchor::parse_place(&d.wb, &a, d.wb.active_sheet) else { continue };
        if pl.sheet != cells.sheet_index {
            continue;
        }
        let r = cells.rect(pl.range);
        painter.rect_stroke(r, 0.0, Stroke::new(1.5, red), egui::StrokeKind::Inside);
        crate::voice_bubble(painter, pos2(r.left() + 1.0, r.top() + 1.0), n, red);
    }
}

#[cfg(test)]
mod tests {
    // Native-only tests: the clock is fine here.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::disallowed_methods)]
    use super::*;
    use gridcraft_agent::layer::{self, QuestionStatus};

    #[test]
    fn numbering_matches_the_splice() {
        let ps = vec![
            Pointer { at: 2.0, anchor: "Sheet1!C3".to_string() },
            Pointer { at: 0.5, anchor: "Sheet1!B2".to_string() },
            Pointer { at: 3.0, anchor: "Sheet1!B2".to_string() },
        ];
        let t =
            craft_voice::Transcript::from_segments(vec![craft_voice::Segment { start: 0.0, end: 4.0, text: "this and that and this".into() }], "en");
        assert_eq!(numbered(&ps), craft_voice::splice(&t, &ps).refs);
    }

    /// The app's session as the agent's MCP server reaches it.
    struct Local<'a>(&'a mut Session);
    impl gridcraft_mcp::Backend for Local<'_> {
        fn call(&mut self, method: &str, params: Json) -> Result<Json, String> {
            match method {
                "engine.execute" => self.0.run(params["command"].as_str().unwrap_or(""), params["params"].clone()),
                _ => Err("unknown".into()),
            }
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    fn app() -> SheetApp {
        crate::install();
        set_settings_path(Some(std::env::temp_dir().join(format!("gridcraft-voice-test-{}.json", std::process::id()))));
        let mut s = Session::new();
        s.new_workbook();
        for (cell, input) in [
            ("A2", json!("Revenue")),
            ("B2", json!(1_250_000)),
            ("A3", json!("Costs")),
            ("B3", json!(870_000)),
            ("A4", json!("Margin")),
            ("B4", json!("=B2-B3")),
        ] {
            s.execute("cell.set", json!({"cell": cell, "input": input})).unwrap();
        }
        // The costs are client data: the agent must see a stand-in.
        s.execute("agent.private", json!({"range": "B3"})).unwrap();
        SheetApp::new(s, Default::default())
    }

    fn status(app: &SheetApp) -> QuestionStatus {
        layer::get(app.session.active().unwrap()).questions.first().map(|q| q.status).unwrap()
    }

    /// Speech made by macOS `say` (none elsewhere).
    fn say(text: &str) -> Option<Audio> {
        let dir = std::env::temp_dir().join(format!("gridcraft-voice-say-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let wav = dir.join("q.wav");
        let ok = std::process::Command::new("say")
            .args(["-v", "Samantha", "-o"])
            .arg(&wav)
            .args(["--data-format=LEI16@16000", text])
            .status()
            .ok()?
            .success();
        let audio = ok.then(|| std::fs::read(&wav).ok()).flatten().and_then(|b| Audio::from_wav(&b).ok());
        let _ = std::fs::remove_dir_all(&dir);
        audio
    }

    #[test]
    fn with_voice_off_a_recording_becomes_a_failed_flag_not_a_dialog() {
        let mut app = app();
        let ctx = egui::Context::default();
        let mut v = Voice { settings_read: true, ..Voice::default() };
        let doc = app.session.active().unwrap().uid;
        v.submit(
            &mut app,
            &ctx,
            doc,
            "Sheet1!B4".into(),
            vec![Pointer { at: 0.2, anchor: "Sheet1!B2".into() }],
            Audio { samples: vec![0.1; 16_000] },
        );
        assert_eq!(status(&app), QuestionStatus::Failed);
        assert!(app.message.is_none());
        // Dismissed: gone, and its audio with it.
        let q = layer::get(app.session.active().unwrap()).questions[0].id.clone();
        v.dismiss(&mut app, &q);
        assert!(layer::get(app.session.active().unwrap()).questions.is_empty());
        assert!(v.jobs.is_empty());
    }

    /// The whole path: speech → Whisper → text with markers → a question with refs → what the
    /// agent reads over MCP. Runs when the small model is downloaded and `say` exists.
    #[test]
    fn speech_with_pointing_reaches_the_agent() {
        let ready = craft_voice::default_dir().is_some_and(|d| Model::Small.is_ready(&d));
        let Some(audio) = ready.then(|| say("Why is the revenue here so much higher than these costs?")).flatten() else {
            eprintln!("skipped: needs the small model in the models folder and macOS `say`");
            return;
        };
        let mut app = app();
        let ctx = egui::Context::default();
        let mut v = Voice { settings_read: true, ..Voice::default() };
        v.apply(&mut app, &ctx, Some(Model::Small));
        let doc = app.session.active().unwrap().uid;
        let secs = audio.seconds();
        // Revenue pointed at a third of the way in, the costs near the end.
        let pointers =
            vec![Pointer { at: secs * 0.35, anchor: "Sheet1!B2".to_string() }, Pointer { at: secs * 0.9, anchor: "Sheet1!B3".to_string() }];
        v.submit(&mut app, &ctx, doc, "Sheet1!B4".into(), pointers, audio);
        // On the cells at once, not with the agent yet.
        assert_eq!(status(&app), QuestionStatus::Transcribing);
        let none = gridcraft_agent::mcp::call(&mut Local(&mut app.session), "list_questions", &json!({"status": "pending"})).unwrap();
        assert!(none.as_array().unwrap().is_empty());
        let t0 = std::time::Instant::now();
        while status(&app) == QuestionStatus::Transcribing {
            assert!(t0.elapsed().as_secs() < 300, "timed out");
            v.frame(&mut app, &ctx);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(status(&app), QuestionStatus::Pending, "{:?}", layer::get(app.session.active().unwrap()).questions);
        // The audio is gone once the text is there.
        assert!(v.jobs.is_empty());
        let list = gridcraft_agent::mcp::call(&mut Local(&mut app.session), "list_questions", &json!({"status": "pending"})).unwrap();
        let q = &list[0];
        let spoken = q["spokenText"].as_str().unwrap();
        let text = q["text"].as_str().unwrap();
        eprintln!("spoken: {spoken}\nagent reads: {text}");
        assert!(spoken.to_lowercase().contains("revenue"), "{spoken}");
        assert_eq!(craft_voice::markers_in(spoken), vec![1, 2], "{spoken}");
        assert!(text.contains("⟦1: Sheet1!B2 · Revenue = 1250000⟧"), "{text}");
        assert!(text.contains("⟦2: Sheet1!B3 · Costs = "), "{text}");
        // Client data: the agent never sees the real costs.
        assert!(!text.contains("870000") && !text.contains("870,000"), "{text}");
        assert_eq!(q["range"], json!("Sheet1!B4"));
        assert_eq!(q["refs"][1]["range"], json!("Sheet1!B3"));
    }
}
