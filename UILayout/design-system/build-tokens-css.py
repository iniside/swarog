"""Compiles tokens.json into tokens.css, laid out like the design-system page's own output."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
T = json.load(open(os.path.join(HERE, 'tokens.json'), encoding='utf-8'))
themes = [t['id'] for t in T['color']['themes']]


def per_theme(tokens, theme):
    out = []
    for t in tokens:
        v = t['value']
        v = v if isinstance(v, str) else v.get(theme, v[themes[0]])
        if v.startswith('{') and v.endswith('}'):
            v = f'var(--{v[1:-1]})'
        out.append(f'  --{t["name"]}: {v};')
    return out


themed = T['color']['tokens'] + T.get('shadow', {}).get('tokens', [])
lines = [f'/* {T["name"]} — generated from tokens.json */']
lines.append(f':root, [data-theme="{themes[0]}"] {{')
lines += per_theme(themed, themes[0])
lines.append('}')
for th in themes[1:]:
    lines.append(f'[data-theme="{th}"] {{')
    lines += per_theme(themed, th)
    lines.append(f'  color-scheme: {th};')
    lines.append('}')
lines.append(':root {')
for fam, body in T.items():
    if fam in ('name', 'version', 'meta', 'color', 'type', 'shadow') or not isinstance(body, dict) or 'tokens' not in body:
        continue
    for t in body['tokens']:
        lines.append('  --' + t['name'].replace('.', '\\.') + f': {t["value"]};')
for key, stack in T['type']['families'].items():
    lines.append(f'  --font-{key}: {stack};')
lines.append('}')
for g in T['type']['groups']:
    for s in g['styles']:
        fam = s.get('family', g['family'])
        decl = [f'font-family: var(--font-{fam})', f'font-size: {s["fontSize"]}']
        if 'lineHeight' in s:
            decl.append(f'line-height: {s["lineHeight"]}')
        if 'fontWeight' in s:
            decl.append(f'font-weight: {s["fontWeight"]}')
        if 'letterSpacing' in s:
            decl.append(f'letter-spacing: {s["letterSpacing"]}')
        lines.append(f'.{s["name"]} {{ ' + '; '.join(decl) + '; }')
open(os.path.join(HERE, 'tokens.css'), 'w', encoding='utf-8', newline='\n').write('\n'.join(lines) + '\n')
