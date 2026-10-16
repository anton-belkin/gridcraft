//! The Agent tab's Context group, its logic: what the agent knows about the document, whether it
//! is ready, and the steps to get it ready. The app checks by itself; when something listens
//! without having answered yet, it asks it quietly (an internal check nobody sees).

use std::collections::{HashMap, HashSet};

use gridcraft_agent::context::{self as cx, Setup};
use gridcraft_agent::layer::{self, AgentContext, CodeVia, ContextKind};
use gridcraft_agent::runner::{self, Presence};
use gridcraft_ui_egui::SheetApp;
use serde_json::json;

use crate::AgentUi;

/// A step of Get ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Step {
    LogIn,
    InstallDesktop,
    AskCowork,
    InstallCode,
    AskSession,
    ChooseFolder,
}

impl Step {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Step::LogIn => "Log in to Claude",
            Step::InstallDesktop | Step::InstallCode => "Install AItoCraft",
            Step::AskCowork => "Ask Cowork to collaborate",
            Step::AskSession => "Ask my session to collaborate",
            Step::ChooseFolder => "Choose folder…",
        }
    }
    pub(crate) fn tip(self) -> &'static str {
        match self {
            Step::LogIn => "Opens a terminal with `claude auth login`",
            Step::InstallDesktop => "Adds AItoCraft to Claude Desktop, where Cowork runs: one connection for every Craft app",
            Step::AskCowork => "Copies the phrase for Claude: paste it into your Cowork chat",
            Step::InstallCode => "Adds AItoCraft to Claude Code for every project (claude mcp add --scope user aitocraft)",
            Step::AskSession => "Copies the command that starts Claude Code with the AItoCraft channel: run it in a terminal",
            Step::ChooseFolder => "The agent runs in that folder: it sees the folder's CLAUDE.md, memory and files",
        }
    }
}

/// The Ready lamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lamp {
    Ready,
    Waiting,
    NotReady,
}

impl Lamp {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Lamp::Ready => "Ready",
            Lamp::Waiting => "Waiting",
            Lamp::NotReady => "Not ready",
        }
    }
}

/// What the person did to get a document ready.
#[derive(Default)]
pub(crate) struct State {
    /// A Get ready step was taken (ms): Waiting until a listener attaches.
    waiting: HashMap<u64, u64>,
    /// Phrases and commands copied, per document.
    copied: HashSet<(u64, Step)>,
    /// The last quiet check, per document.
    checked: HashMap<u64, u64>,
}

/// Waiting lasts this long after a step.
const WAIT_MS: u64 = 10 * 60_000;
/// A listener seen this recently makes the document ready.
const SEEN_MS: u64 = 5 * 60_000;
/// A quiet check at most this often.
const CHECK_EVERY_MS: u64 = 2 * 60_000;

/// The group as drawn.
pub(crate) struct View {
    pub ctx: AgentContext,
    pub lamp: Lamp,
    pub detail: String,
    /// Every step of Get ready with whether it's done; empty: no Get ready.
    pub steps: Vec<(Step, bool)>,
    /// The chosen project folder, when it's there.
    pub folder: Option<String>,
}

impl View {
    /// What Get ready's main click does: the first step not done (else the last one again).
    pub(crate) fn next(&self) -> Option<Step> {
        self.steps.iter().find(|(_, done)| !done).or(self.steps.last()).map(|(s, _)| *s)
    }
}

fn listener_ready(p: &Presence) -> bool {
    match p {
        Presence::Answering { .. } => true,
        Presence::Listening { seen_ms, .. } => *seen_ms < SEEN_MS,
        _ => false,
    }
}

/// The lamp, its details and the steps, for a context (pure, so it is tested).
pub(crate) fn view(c: &AgentContext, setup: &Setup, presence: &Presence, waiting: bool, copied: &dyn Fn(Step) -> bool, server: &str) -> View {
    let folder = (!c.folder.is_empty() && std::path::Path::new(&c.folder).is_dir()).then(|| c.folder.clone());
    let background = |extra: &str| {
        if setup.logged_in {
            (Lamp::Ready, format!("Claude is logged in: the background client starts with the first question.{extra}"))
        } else if waiting {
            (Lamp::Waiting, "Waiting for Claude to be logged in.".to_string())
        } else if setup.checked_at == 0 {
            (Lamp::NotReady, "Checking Claude on this computer…".to_string())
        } else if !setup.cli {
            (Lamp::NotReady, "Claude Code isn't installed on this computer (the `claude` command).".to_string())
        } else {
            (Lamp::NotReady, "Claude isn't logged in on this computer.".to_string())
        }
    };
    let listening = |who: &str| {
        let ready = listener_ready(presence);
        if ready {
            let by = match presence {
                Presence::Listening { label, .. } => format!(" ({label})"),
                _ => String::new(),
            };
            (Lamp::Ready, format!("{who} works on this document{by}: questions reach it as you ask them."))
        } else if waiting || matches!(presence, Presence::Checking { .. } | Presence::Listener(_)) {
            (Lamp::Waiting, format!("Waiting for {who} to attach to this document."))
        } else {
            (Lamp::NotReady, format!("{who} isn't attached to this document yet. Get ready shows the steps."))
        }
    };
    let login = (!setup.logged_in).then_some((Step::LogIn, false));
    let (lamp, detail, steps) = match (c.kind, c.via) {
        (ContextKind::None, _) => {
            let (l, d) = background(if c.web { " It may also search and read the web." } else { " It sees only this document." });
            (l, d, login.into_iter().collect())
        }
        (ContextKind::Cowork, _) => {
            let (l, d) = listening("Your Cowork chat");
            let asked = copied(Step::AskCowork) || l == Lamp::Ready;
            (l, d, vec![(Step::InstallDesktop, setup.desktop_hub), (Step::AskCowork, asked)])
        }
        (ContextKind::Code, CodeVia::Session) => {
            let (l, d) = listening("Your Claude Code session");
            let asked = copied(Step::AskSession) || l == Lamp::Ready;
            (l, d, vec![(Step::InstallCode, setup.code_hub), (Step::AskSession, asked)])
        }
        (ContextKind::Code, CodeVia::Folder) => {
            let (l, d) = match &folder {
                Some(f) => background(&format!(" It runs in {f}.")),
                None => (if waiting { Lamp::Waiting } else { Lamp::NotReady }, "Choose the project folder the agent runs in.".to_string()),
            };
            let mut steps: Vec<(Step, bool)> = login.into_iter().collect();
            steps.push((Step::ChooseFolder, folder.is_some()));
            (l, d, steps)
        }
        (ContextKind::Off, _) => (Lamp::NotReady, "No agent: its tools refuse and nothing is sent.".to_string(), vec![]),
    };
    let detail = format!("{detail}\n\nGridCraft's MCP server: {server}");
    View { ctx: c.clone(), lamp, detail, steps, folder }
}

/// The active document's group.
pub(crate) fn current(st: &AgentUi, app: &SheetApp) -> Option<View> {
    let d = app.session.active()?;
    let c = cx::effective(&layer::get(d));
    let setup = cx::setup();
    let presence = runner::presence(&app.session);
    let now = gridcraft_agent::now_ms();
    let waiting = st.context.waiting.get(&d.uid).is_some_and(|t| now.saturating_sub(*t) < WAIT_MS);
    let uid = d.uid;
    let copied = |s: Step| st.context.copied.contains(&(uid, s));
    Some(view(&c, &setup, &presence, waiting, &copied, crate::server_url().unwrap_or("http://127.0.0.1:7980/mcp")))
}

/// Once per frame: the runner follows the active document's context; something listening that
/// hasn't answered is asked quietly; while waiting, the setup is checked more often.
pub(crate) fn frame(st: &mut AgentUi, app: &mut SheetApp, ctx: &egui::Context) {
    cx::sync(&app.session);
    for msg in [cx::take_result(), gridcraft_agent::hub::take_result()].into_iter().flatten() {
        app.message = Some(("AItoCraft".into(), msg));
    }
    let Some(d) = app.session.active() else { return };
    let uid = d.uid;
    let l = layer::get(d);
    let c = cx::effective(&l);
    let now = gridcraft_agent::now_ms();
    let presence = runner::presence(&app.session);
    if listener_ready(&presence) {
        st.context.waiting.remove(&uid);
    }
    let waiting = st.context.waiting.get(&uid).is_some_and(|t| now.saturating_sub(*t) < WAIT_MS);
    if waiting {
        if now.saturating_sub(cx::setup().checked_at) > 5_000 {
            cx::recheck();
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }
    // Cowork and my session: something listens but hasn't answered for this document yet.
    let listens = matches!(c.kind, ContextKind::Cowork) || (c.kind == ContextKind::Code && c.via == CodeVia::Session);
    if listens && !gridcraft_agent::read_only() {
        let quiet = runner::classify(None, &runner::signals(uid), runner::mode(), now);
        let unsure = matches!(quiet, Presence::Listener(_)) && !listener_ready(&presence) && !matches!(presence, Presence::Checking { .. });
        let last = st.context.checked.get(&uid).copied().unwrap_or(0);
        if unsure && now.saturating_sub(last) > CHECK_EVERY_MS {
            st.context.checked.insert(uid, now);
            let _ = app.run("agent.ping", json!({}));
        }
        if !matches!(presence, Presence::Nobody) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

/// Runs a step of Get ready.
pub(crate) fn run(st: &mut AgentUi, app: &mut SheetApp, ctx: &egui::Context, step: Step) {
    let Some(d) = app.session.active() else { return };
    let uid = d.uid;
    let title = d.display_title();
    let now = gridcraft_agent::now_ms();
    let wait = |st: &mut AgentUi| {
        st.context.waiting.insert(uid, now);
    };
    match step {
        Step::LogIn => match cx::log_in() {
            Ok(()) => wait(st),
            Err(e) => app.message = Some(("Log in to Claude".into(), e)),
        },
        Step::InstallDesktop => match gridcraft_agent::hub::install_desktop() {
            Ok(()) => {
                wait(st);
                app.toast = Some(("Installing AItoCraft in Claude Desktop…".into(), gridcraft_ui_egui::now_ms()));
            }
            Err(e) => app.message = Some(("AItoCraft".into(), e)),
        },
        Step::InstallCode => match cx::install_code() {
            Ok(()) => {
                wait(st);
                app.toast = Some(("Installing AItoCraft in Claude Code…".into(), gridcraft_ui_egui::now_ms()));
            }
            Err(e) => app.message = Some(("AItoCraft".into(), e)),
        },
        Step::AskCowork | Step::AskSession => {
            let (text, toast) = if step == Step::AskCowork {
                (gridcraft_agent::hub::phrase(&title), "Copied: paste it into your Cowork chat")
            } else {
                (gridcraft_agent::hub::CHANNEL_COMMAND.to_string(), "Copied: run it in a terminal to start Claude Code with the channel")
            };
            ctx.copy_text(text);
            st.context.copied.insert((uid, step));
            wait(st);
            app.toast = Some((toast.into(), gridcraft_ui_egui::now_ms()));
        }
        Step::ChooseFolder => {
            if let Some(f) = crate::pick_folder() {
                app.run_or_alert("agent.agentContext", json!({"kind": "code", "via": "folder", "folder": f}));
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx(kind: ContextKind, via: CodeVia, folder: &str) -> AgentContext {
        AgentContext { kind, via, folder: folder.into(), ..AgentContext::default() }
    }

    fn checked(logged_in: bool) -> Setup {
        Setup { cli: true, logged_in, checked_at: 1, ..Setup::default() }
    }

    #[test]
    fn no_context_needs_only_a_login() {
        let none = ctx(ContextKind::None, CodeVia::Session, "");
        let v = view(&none, &checked(false), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.lamp, Lamp::NotReady);
        assert_eq!(v.steps, vec![(Step::LogIn, false)]);
        assert_eq!(v.next(), Some(Step::LogIn));
        // After the step: waiting until Claude is logged in.
        assert_eq!(view(&none, &checked(false), &Presence::Nobody, true, &|_| false, "url").lamp, Lamp::Waiting);
        // Logged in: ready, and no Get ready at all.
        let v = view(&none, &checked(true), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.lamp, Lamp::Ready);
        assert!(v.steps.is_empty());
    }

    #[test]
    fn cowork_installs_then_asks_and_is_ready_when_its_chat_listens() {
        let c = ctx(ContextKind::Cowork, CodeVia::Session, "");
        let v = view(&c, &checked(true), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.lamp, Lamp::NotReady);
        assert_eq!(v.next(), Some(Step::InstallDesktop));
        let installed = Setup { desktop_hub: true, ..checked(true) };
        let v = view(&c, &installed, &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.next(), Some(Step::AskCowork));
        assert_eq!(v.steps, vec![(Step::InstallDesktop, true), (Step::AskCowork, false)]);
        // The phrase copied: waiting for the chat to attach.
        let v = view(&c, &installed, &Presence::Nobody, true, &|s| s == Step::AskCowork, "url");
        assert_eq!(v.lamp, Lamp::Waiting);
        assert!(v.steps.iter().all(|(_, d)| *d));
        // Something listens without having answered: still waiting (the app checks quietly).
        assert_eq!(view(&c, &installed, &Presence::Listener(vec!["waiting for questions"]), false, &|_| false, "url").lamp, Lamp::Waiting);
        // The chat attached: ready, every step ticked.
        let attached = Presence::Listening { label: "Cowork · Northwind".into(), seen_ms: 4_000, more: 0 };
        let v = view(&c, &installed, &attached, false, &|_| false, "url");
        assert_eq!(v.lamp, Lamp::Ready);
        assert!(v.steps.iter().all(|(_, d)| *d));
        assert!(v.detail.contains("Cowork · Northwind"));
    }

    #[test]
    fn claude_code_session_or_folder() {
        let session = ctx(ContextKind::Code, CodeVia::Session, "");
        let v = view(&session, &checked(true), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.steps, vec![(Step::InstallCode, false), (Step::AskSession, false)]);
        let answering = Presence::Answering { checked_ms: 2_000, latency_ms: 900 };
        assert_eq!(view(&session, &checked(true), &answering, false, &|_| false, "url").lamp, Lamp::Ready);
        // A project folder: chosen, it is shown and the background client is ready.
        let none_yet = ctx(ContextKind::Code, CodeVia::Folder, "");
        let v = view(&none_yet, &checked(true), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!((v.lamp, v.next()), (Lamp::NotReady, Some(Step::ChooseFolder)));
        let dir = std::env::temp_dir().display().to_string();
        let v = view(&ctx(ContextKind::Code, CodeVia::Folder, &dir), &checked(true), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.lamp, Lamp::Ready);
        assert_eq!(v.folder.as_deref(), Some(dir.as_str()));
        assert_eq!(v.steps, vec![(Step::ChooseFolder, true)]);
        // Not logged in: that comes first.
        let v = view(&ctx(ContextKind::Code, CodeVia::Folder, &dir), &checked(false), &Presence::Nobody, false, &|_| false, "url");
        assert_eq!(v.next(), Some(Step::LogIn));
    }

    #[test]
    fn the_checks_stay_internal() {
        let all = [
            ctx(ContextKind::None, CodeVia::Session, ""),
            ctx(ContextKind::Cowork, CodeVia::Session, ""),
            ctx(ContextKind::Code, CodeVia::Session, ""),
            ctx(ContextKind::Code, CodeVia::Folder, ""),
        ];
        let presences = [
            Presence::Nobody,
            Presence::Checking { since_ms: 1_000 },
            Presence::NoAnswer { asked_ms: 70_000 },
            Presence::Answering { checked_ms: 1_000, latency_ms: 500 },
            Presence::Listener(vec!["event stream"]),
        ];
        for c in &all {
            for p in &presences {
                for setup in [checked(false), checked(true), Setup::default()] {
                    let v = view(c, &setup, p, false, &|_| false, "url");
                    let words = format!(
                        "{} {} {}",
                        v.detail,
                        v.lamp.label(),
                        v.steps.iter().map(|(s, _)| format!("{} {}", s.label(), s.tip())).collect::<String>()
                    )
                    .to_lowercase();
                    assert!(!words.contains("ping") && !words.contains("anyone there"), "{words}");
                }
            }
        }
    }
}
