#!/usr/bin/env python3
"""Derive two-color SVG templates while retaining geometry and mask semantics."""
import copy, hashlib, json, pathlib, re, xml.etree.ElementTree as ET
root = pathlib.Path(__file__).resolve().parents[1] / 'assets/logos'
manifest = json.loads((root / 'harness-logos.json').read_text())
INK, PAPER = '#123456', '#fedcba'

def paint(value):
    value = value.strip()
    if value.lower() in ('white', '#fff', '#ffffff'): return PAPER
    if value.lower() in ('black', 'currentcolor'): return INK
    if re.fullmatch(r'#[0-9a-fA-F]{3}(?:[0-9a-fA-F]{3})?', value):
        digits = value[1:]
        if len(digits) == 3: digits = ''.join(c * 2 for c in digits)
        rgb = [int(digits[i:i+2], 16) for i in (0, 2, 4)]
        return PAPER if min(rgb) >= 220 and max(rgb) - min(rgb) < 25 else INK
    return value

def css(text):
    return re.sub(r'((?:fill|stroke|stop-color|color)\s*:\s*)([^;}]+)', lambda m: m[1] + paint(m[2]), text)

def visit(element):
    # Luminance masks and clip geometry are not visible artwork. Recoloring
    # their white/black pixels would make a logo disappear on a dark theme.
    if element.tag.split('}')[-1] in ('mask', 'clipPath', 'filter'): return
    for key in ('fill', 'stroke', 'stop-color', 'color'):
        if key in element.attrib: element.set(key, paint(element.get(key)))
    if 'style' in element.attrib: element.set('style', css(element.get('style')))
    if element.tag.split('}')[-1] == 'style' and element.text: element.text = css(element.text)
    for child in element: visit(child)

ET.register_namespace('', 'http://www.w3.org/2000/svg')
for key, entry in manifest.items():
    svg = ET.fromstring((root / entry['light']).read_bytes())
    # Explicit interior roles preserve eyes, screen shading and negative space.
    details = []
    if key == 'antigravity':
        # The upstream alpha mask is the exact standalone product silhouette.
        # Use that path directly instead of its overlapping blurred color blobs.
        mask = next(e for e in svg.iter() if e.tag.split('}')[-1] == 'mask')
        silhouette = copy.deepcopy(list(mask)[0])
        silhouette.set('fill', 'black')
        for child in list(svg): svg.remove(child)
        svg.append(silhouette)
    for element in svg.iter():
        value = element.get('fill', '').lower()
        if key == 'openclaw' and value == '#050810': details.append((element, PAPER, None))
        if key == 'crush' and value in ('#ff13a9', '#ff388b'): details.append((element, PAPER, '0.45'))
        if key == 'opencode' and value == '#cfcecd': details.append((element, INK, '0.35'))
    visit(svg)
    for element, color, opacity in details:
        element.set('fill', color)
        if opacity: element.set('opacity', opacity)
    svg.set('color', INK)
    if 'fill' not in svg.attrib: svg.set('fill', PAPER if key == 'crush' else INK)
    # Monochrome geometry does not need color blending; AndroidSVG cannot blur.
    # Keep the clipping mask, remove only filter effects on visible shapes.
    for element in svg.iter(): element.attrib.pop('filter', None)
    name = f'{key}-themed.svg'
    data = ET.tostring(svg, encoding='utf-8')
    (root / name).write_bytes(data)
    entry['themed'] = name
    entry['files'][name] = hashlib.sha256(data).hexdigest()
(root / 'harness-logos.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(f'Generated {len(manifest)} SVG templates: {INK}=theme ink, {PAPER}=theme surface.')
