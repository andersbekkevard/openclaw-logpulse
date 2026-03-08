from pathlib import Path
from html import escape
TMP = Path('/home/anders/.openclaw/workspace/dev/openclaw-logpulse/.tmp')

def one(title, txt):
    return f'''<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{escape(title)}</title>
<style>
:root{{color-scheme:dark}} body{{margin:0;background:linear-gradient(180deg,#0b1020 0%,#111827 100%);font-family:Inter,system-ui,sans-serif;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:16px;box-sizing:border-box}}
.frame{{width:min(100%,420px);border-radius:18px;overflow:hidden;box-shadow:0 20px 60px rgba(0,0,0,.45);border:1px solid rgba(148,163,184,.22);background:#0b1220}}
.bar{{height:44px;display:flex;align-items:center;gap:10px;padding:0 14px;background:#1f2937;color:#e5e7eb;font-size:14px;font-weight:600}}
.dot{{width:11px;height:11px;border-radius:999px}} .red{{background:#ff5f57}} .yellow{{background:#febc2e}} .green{{background:#28c840}}
pre{{margin:0;padding:14px 16px 18px;color:#d1d5db;background:#0b1220;font:9px/1.18 "DejaVu Sans Mono","Liberation Mono",Menlo,Consolas,monospace;white-space:pre;overflow:hidden}}
</style></head><body><div class="frame"><div class="bar"><span class="dot red"></span><span class="dot yellow"></span><span class="dot green"></span><span style="margin-left:8px">{escape(title)}</span></div><pre>{escape(txt)}</pre></div></body></html>'''

for src, out, title in [
    ('mobile-events.txt','logpulse-mobile-events.html','logpulse mobile events'),
    ('mobile-detail.txt','logpulse-mobile-detail.html','logpulse mobile detail'),
]:
    txt=(TMP/src).read_text()
    (TMP/out).write_text(one(title, txt))
    print(TMP/out)
