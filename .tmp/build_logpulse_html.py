from pathlib import Path
from html import escape

TMP = Path('/home/anders/.openclaw/workspace/dev/openclaw-logpulse/.tmp')

def render_terminal(title: str, content: str) -> str:
    return f'''<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{escape(title)}</title>
<style>
:root {{ color-scheme: dark; }}
body {{ margin:0; background:linear-gradient(180deg,#0b1020 0%,#111827 100%); font-family:Inter,system-ui,sans-serif; min-height:100vh; display:flex; align-items:center; justify-content:center; padding:24px; box-sizing:border-box; }}
.frame {{ width:fit-content; max-width:100%; border-radius:18px; overflow:hidden; box-shadow:0 20px 60px rgba(0,0,0,.45); border:1px solid rgba(148,163,184,.22); background:#0b1220; }}
.bar {{ height:46px; display:flex; align-items:center; gap:10px; padding:0 16px; background:#1f2937; color:#e5e7eb; font-size:14px; font-weight:600; }}
.dot {{ width:12px; height:12px; border-radius:999px; }} .red{{background:#ff5f57}} .yellow{{background:#febc2e}} .green{{background:#28c840}}
pre {{ margin:0; padding:18px 22px 22px; color:#d1d5db; background:#0b1220; font:16px/1.25 "DejaVu Sans Mono","Liberation Mono",Menlo,Consolas,monospace; white-space:pre; overflow:auto; }}
.grid {{ display:grid; grid-template-columns:1fr 1fr; gap:18px; max-width:1800px; }}
.cardtitle {{ color:#cbd5e1; font:600 15px Inter,system-ui,sans-serif; padding:0 0 8px 4px; }}
.wrap {{ width:min(100%,1800px); }}
@media (max-width: 1100px) {{ .grid {{ grid-template-columns:1fr; }} pre {{ font-size:14px; }} }}
</style></head><body><div class="frame"><div class="bar"><span class="dot red"></span><span class="dot yellow"></span><span class="dot green"></span><span style="margin-left:8px">{escape(title)}</span></div><pre>{escape(content)}</pre></div></body></html>'''

def card_html(title: str, content: str) -> str:
    return f'''<div><div class="cardtitle">{escape(title)}</div><div class="frame"><div class="bar"><span class="dot red"></span><span class="dot yellow"></span><span class="dot green"></span><span style="margin-left:8px">{escape(title)}</span></div><pre>{escape(content)}</pre></div></div>'''

def render_collage(pairs):
    cards = ''.join(card_html(t, c) for t, c in pairs)
    return f'''<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>logpulse collage</title>
<style>
:root {{ color-scheme: dark; }}
body {{ margin:0; background:linear-gradient(180deg,#0b1020 0%,#111827 100%); font-family:Inter,system-ui,sans-serif; min-height:100vh; padding:24px; box-sizing:border-box; }}
.frame {{ width:fit-content; max-width:100%; border-radius:18px; overflow:hidden; box-shadow:0 20px 60px rgba(0,0,0,.45); border:1px solid rgba(148,163,184,.22); background:#0b1220; }}
.bar {{ height:46px; display:flex; align-items:center; gap:10px; padding:0 16px; background:#1f2937; color:#e5e7eb; font-size:14px; font-weight:600; }}
.dot {{ width:12px; height:12px; border-radius:999px; }} .red{{background:#ff5f57}} .yellow{{background:#febc2e}} .green{{background:#28c840}}
pre {{ margin:0; padding:18px 22px 22px; color:#d1d5db; background:#0b1220; font:12px/1.18 "DejaVu Sans Mono","Liberation Mono",Menlo,Consolas,monospace; white-space:pre; overflow:auto; }}
.grid {{ display:grid; grid-template-columns:1fr 1fr; gap:18px; max-width:1800px; margin:0 auto; }}
.cardtitle {{ color:#cbd5e1; font:600 15px Inter,system-ui,sans-serif; padding:0 0 8px 4px; }}
.hero {{ color:#e5e7eb; font:700 22px Inter,system-ui,sans-serif; max-width:1800px; margin:0 auto 18px; }}
.sub {{ color:#94a3b8; font:500 14px Inter,system-ui,sans-serif; margin-top:4px; }}
@media (max-width: 1100px) {{ .grid {{ grid-template-columns:1fr; }} pre {{ font-size:13px; }} }}
</style></head><body><div class="hero">openclaw-logpulse demo<div class="sub">Events, Correlated Tool Calls, Sessions, and fullscreen detail</div></div><div class="grid">{cards}</div></body></html>'''

pairs = []
for name, title in [('events2','Events'),('calls2','Tool Calls'),('sessions2','Sessions'),('detail2','Fullscreen Detail')]:
    txt = (TMP/f'{name}.txt').read_text()
    pairs.append((title, txt))
    if name == 'detail2':
        (TMP/'logpulse-detail.html').write_text(render_terminal('logpulse detail view', txt))

(TMP/'logpulse-collage.html').write_text(render_collage(pairs))
print(TMP/'logpulse-collage.html')
print(TMP/'logpulse-detail.html')
