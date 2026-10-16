#!/usr/bin/env bash
# Builds the demo workbook (a small SaaS revenue model with an agent layer) and renders the Agent
# pane offscreen. Usage: agent-layer/demo/build-demo.sh <out-dir>
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
out="${1:-/tmp/agent-demo}"
mkdir -p "$out"
snap() { cargo run --release -q -p gridcraft-agent-ui --example agent_snapshot -- "$@"; }
values='[["Revenue model 2027",""],["Client","Northwind Logistics GmbH"],["Customers at start",1200],["New customers / month",85],["Monthly churn",0.021],["Price per seat / month",49],["Seats per customer",14],["Gross margin",0.78],["Sales & marketing",2400000],["R&D",1900000],["G&A",900000],["",""],["Customers at year end","=B3*(1-B5)^12+B4*(1-(1-B5)^12)/B5"],["Average customers","=(B3+B13)/2"],["Revenue","=B14*B7*B6*12"],["Gross profit","=B15*B8"],["Operating costs","=B9+B10+B11"],["EBITDA","=B16-B17"],["EBITDA margin","=B18/B15"]]'
cmds=()
agent() { cmds+=(--cmd "$1=$2"); }

agent range.setValues "{\"range\":\"A1\",\"values\":$values}"
agent home.bold '{"range":"A1"}'
agent home.columnWidth '{"cols":"A:A","chars":24}'
agent home.columnWidth '{"cols":"B:B","chars":13}'
agent home.numberFormat '{"range":"B5","code":"0.0%"}'
agent home.numberFormat '{"range":"B8","code":"0%"}'
agent home.numberFormat '{"range":"B19","code":"0.0%"}'
agent home.numberFormat '{"range":"B9:B11","code":"#,##0"}'
agent home.numberFormat '{"range":"B13:B18","code":"#,##0"}'
agent agent.private '{"range":"B2:B3"}'
agent agent.input '{"cell":"B4","low":60,"high":110,"rationale":"Pipeline: 60 is last year’s run rate, 110 needs the partner channel live by March.","actor":"agent"}'
agent agent.input '{"cell":"B5","low":0.015,"high":0.035,"rationale":"Churn of comparable logistics SaaS: 1.5–3.5% a month.","actor":"agent"}'
agent agent.input '{"cell":"B6","low":44,"high":52,"rationale":"List price 52; average discount up to 15%.","actor":"agent"}'
agent agent.input '{"cell":"B7","low":12,"high":16,"rationale":"Seat count of signed customers.","actor":"agent"}'
agent agent.input '{"cell":"B8","low":0.72,"high":0.82,"rationale":"Hosting cost per seat may rise with the new region.","actor":"agent"}'
agent agent.input '{"cell":"B9","low":0.95,"high":1.2,"relative":true,"rationale":"Hiring plan for two more AEs not approved yet.","actor":"agent"}'
agent agent.confidence '{"range":"B4","level":0.35,"reason":"Partner channel not signed","actor":"agent"}'
agent agent.confidence '{"range":"B5","level":0.7,"reason":"12 months of history"}'
agent agent.confidence '{"range":"B6","level":0.9,"reason":"Price list 2027"}'
agent agent.confidence '{"range":"B7","level":0.55,"reason":"Seats grow slower in Q1"}'
agent agent.confidence '{"range":"B8","level":0.8,"reason":"Hosting contract"}'
agent agent.confidence '{"range":"B9","level":0.6,"reason":"Budget draft"}'
agent agent.variants '{"range":"B4","title":"New customers / month","current":"Base 85","options":[{"name":"Partner 110","values":110,"note":"With the partner channel live from March."},{"name":"Cautious 60","values":60,"note":"Last year’s run rate."}],"actor":"agent"}'
agent agent.method '{"cell":"B14","exact":"=AVERAGE(B3*(1-B5)^SEQUENCE(12)+B4*(1-(1-B5)^SEQUENCE(12))/B5)","title":"Average customers","note":"Month-by-month average; the simple formula assumes a straight line from start to end.","actor":"agent"}'
agent file.saveAs "{\"path\":\"$out/demo.xlsx\"}"

snap "${cmds[@]}" --pane variants "$out/variants.png"
sel() { printf -- 'selection.set={"range":"%s"}' "$1"; }
snap --in "$out/demo.xlsx" --cmd "$(sel B4)" --pane cell "$out/1-variants-cell.png"
snap --in "$out/demo.xlsx" --cmd "$(sel B14)" --pane cell "$out/2-method.png"
snap --in "$out/demo.xlsx" --cmd "$(sel B18)" --pane cell --size 1440x1640 "$out/3-worst-best.png"
snap --in "$out/demo.xlsx" --cmd "$(sel B18)" --pane risk "$out/4-risk.png"
# Safe file: keys in a folder beside (never inside) the workbook's; then the same file where the key isn't.
export GRIDCRAFT_AGENT_KEYS="$out-keys"
snap --in "$out/demo.xlsx" --cmd 'agent.safeMode={}' --cmd "file.saveAs={\"path\":\"$out/safe.xlsx\"}" --cmd "$(sel B3)" --pane cell "$out/5-safe-on.png"
GRIDCRAFT_AGENT_KEYS="$out-elsewhere" snap --in "$out/safe.xlsx" --cmd "$(sel B3)" --pane cell "$out/5-safe-elsewhere.png"
# Excel → slide links: copy a range as a linked source, then a use as DeckCraft reports it.
unset GRIDCRAFT_AGENT_KEYS
snap --in "$out/demo.xlsx" --cmd 'link.copy={"range":"A15:B18"}' --cmd 'file.save={}' "$out/.links.png"
anchor=$(python3 -c "import zipfile,re,sys; print(re.search(r'name=\"(_GcLink_l\d+)\"', zipfile.ZipFile(sys.argv[1]).read('xl/workbook.xml').decode()).group(1))" "$out/demo.xlsx")
use="{\"uses\":[{\"event\":\"use\",\"at\":1,\"app\":\"DeckCraft\",\"link\":\"l18f\",\"workbook\":\"$out/demo.xlsx\",\"anchor\":\"$anchor\",\"sheet\":\"Sheet1\",\"range\":\"A15:B18\",\"deck\":\"$out/board.pptx\",\"deckTitle\":\"Board update Q3\",\"slide\":3,\"slideTitle\":\"Revenue and EBITDA\",\"shapeKey\":\"k4f2a9\",\"mode\":\"table\"}]}"
snap --in "$out/demo.xlsx" --cmd "link.addUses=$use" --cmd "$(sel B17)" --pane cell --hover B18 "$out/6-linked-slides.png"
rm -f "$out/.links.png"
