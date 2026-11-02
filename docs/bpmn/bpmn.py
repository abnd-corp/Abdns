#!/usr/bin/env python3
"""Génère des diagrammes BPMN en PNG à partir de fichiers TOML.

    python3 docs/bpmn/bpmn.py                  # tous les docs/bpmn/*.toml
    python3 docs/bpmn/bpmn.py a.toml --out d/  # un fichier, PNG dans d/

Règles tenues par le générateur (il s'arrête avec une erreur plutôt que de les enfreindre) :
  - un titre en haut de l'image (obligatoire) ;
  - aucune flèche n'en croise une autre, ni ne traverse un élément ;
  - aucun libellé ne chevauche un autre libellé, un élément ou une flèche.

Dépendance : Pillow (voir requirements.txt). Format des fichiers : voir README.md.
"""

import argparse
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# ── Géométrie (unités logiques) ─────────────────────────────────────────────────────────────
COL_W, ROW_H = 230, 150
TASK_W, TASK_MIN_H, TASK_PAD = 160, 64, 14
EVENT_R, GATE_HALF = 20, 26
MARGIN, TITLE_H = 48, 84
CHANNEL_GAP, CHANNEL_STEP = 24, 8
LANE_GAP, LANE_STEP = 34, 16
LABEL_WIDTH = 150
TITLE_SIZE, TEXT_SIZE, FLOW_SIZE = 26, 14, 13
MAX_COMBOS = 512

# ── Rendu ──────────────────────────────────────────────────────────────────────────────────
OUTPUT_SCALE, SUPERSAMPLE = 2, 2
FACTOR = OUTPUT_SCALE * SUPERSAMPLE
LINE = 2
INK, FLOW_INK, BACKGROUND = "#1f2933", "#3e4c59", "#ffffff"
STYLE = {
    "start": ("#e3f6e8", "#2e7d32", 2),
    "end": ("#fde8e6", "#c62828", 4),
    "task": ("#eaf1fb", "#3b5b92", 2),
    "gateway": ("#fff6dc", "#a77b00", 2),
}
KINDS = tuple(STYLE)
MARKERS = ("xor", "parallel")

FONT_DIRS = [
    "/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/dejavu", "/usr/share/fonts/TTF",
    "/usr/share/fonts/truetype/liberation", "/Library/Fonts", "/System/Library/Fonts/Supplemental", "C:/Windows/Fonts",
]
FONT_FILES = {False: ("DejaVuSans.ttf", "LiberationSans-Regular.ttf", "Arial.ttf"),
              True: ("DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf", "Arial Bold.ttf", "arialbd.ttf")}

Rect = tuple[float, float, float, float]
Point = tuple[float, float]


class DiagramError(Exception):
    """Fichier de définition invalide, ou diagramme qui ne peut pas respecter les règles."""


# ── Polices et texte ───────────────────────────────────────────────────────────────────────
_fonts: dict[tuple[float, bool], ImageFont.FreeTypeFont] = {}


def font(size: float, bold: bool = False) -> ImageFont.FreeTypeFont:
    """Police à l'échelle du dessin ; les mesures sont ramenées en unités logiques par `width`."""
    key = (size, bold)
    if key not in _fonts:
        for directory in FONT_DIRS:
            for name in FONT_FILES[bold]:
                try:
                    _fonts[key] = ImageFont.truetype(str(Path(directory) / name), round(size * FACTOR))
                    return _fonts[key]
                except OSError:
                    continue
        _fonts[key] = ImageFont.load_default(round(size * FACTOR))
    return _fonts[key]


def width(text: str, size: float, bold: bool = False) -> float:
    return font(size, bold).getlength(text) / FACTOR


def wrap(text: str, size: float, max_width: float) -> list[str]:
    """Coupe aux espaces ; les retours à la ligne du texte sont conservés."""
    lines: list[str] = []
    for paragraph in text.split("\n"):
        line = ""
        for word in paragraph.split():
            attempt = f"{line} {word}".strip()
            if line and width(attempt, size) > max_width:
                lines.append(line)
                line = word
            else:
                line = attempt
        lines.append(line)
    return lines


def line_height(size: float) -> float:
    return size * 1.3


def text_box(lines: list[str], size: float) -> tuple[float, float]:
    return max(width(l, size) for l in lines), line_height(size) * len(lines)


# ── Modèle ─────────────────────────────────────────────────────────────────────────────────
@dataclass
class Node:
    id: str
    kind: str
    label: str
    marker: str
    index: int
    col_hint: int | None = None
    row_hint: int | None = None
    lines: list[str] = field(default_factory=list)
    w: float = 0
    h: float = 0
    col: int = 0
    cx: float = 0
    cy: float = 0

    @property
    def rect(self) -> Rect:
        return (self.cx - self.w / 2, self.cy - self.h / 2, self.cx + self.w / 2, self.cy + self.h / 2)

    def port(self, side: str) -> Point:
        return {"left": (self.cx - self.w / 2, self.cy), "right": (self.cx + self.w / 2, self.cy),
                "top": (self.cx, self.cy - self.h / 2), "bottom": (self.cx, self.cy + self.h / 2)}[side]


@dataclass
class Flow:
    src: str
    dst: str
    label: str
    index: int
    back: bool = False
    path: list[Point] = field(default_factory=list)
    lines: list[str] = field(default_factory=list)
    label_rect: Rect | None = None

    @property
    def name(self) -> str:
        return f"{self.src} → {self.dst}"


@dataclass
class Model:
    title: str
    nodes: dict[str, Node]
    flows: list[Flow]


def load(path: Path) -> Model:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise DiagramError(f"{path}: {error}") from error
    known = {"title", "node", "flow"}
    if extra := set(data) - known:
        raise DiagramError(f"clé inconnue : {', '.join(sorted(extra))}")
    title = str(data.get("title", "")).strip()
    if not title:
        raise DiagramError("title est obligatoire : un diagramme sans titre n'est pas généré")
    nodes: dict[str, Node] = {}
    for i, raw in enumerate(data.get("node", [])):
        if extra := set(raw) - {"id", "kind", "label", "marker", "col", "row"}:
            raise DiagramError(f"node {i + 1} : clé inconnue : {', '.join(sorted(extra))}")
        node_id, kind = str(raw.get("id", "")), raw.get("kind", "task")
        if not node_id or node_id in nodes:
            raise DiagramError(f"node {i + 1} : id vide ou en double ({node_id!r})")
        if kind not in KINDS:
            raise DiagramError(f"{node_id} : kind doit être l'un de {', '.join(KINDS)}")
        marker = raw.get("marker", "xor")
        if marker not in MARKERS:
            raise DiagramError(f"{node_id} : marker doit être l'un de {', '.join(MARKERS)}")
        label = str(raw.get("label", "")).strip()
        if not label and kind in ("task", "start", "end"):
            raise DiagramError(f"{node_id} : label obligatoire")
        nodes[node_id] = Node(node_id, kind, label, marker, i, raw.get("col"), raw.get("row"))
    flows: list[Flow] = []
    for i, raw in enumerate(data.get("flow", [])):
        if extra := set(raw) - {"from", "to", "label"}:
            raise DiagramError(f"flow {i + 1} : clé inconnue : {', '.join(sorted(extra))}")
        src, dst = str(raw.get("from", "")), str(raw.get("to", ""))
        for end in (src, dst):
            if end not in nodes:
                raise DiagramError(f"flow {i + 1} : nœud inconnu {end!r}")
        if src == dst or any(f.src == src and f.dst == dst for f in flows):
            raise DiagramError(f"flow {i + 1} : {src} → {dst} est une boucle sur soi ou un doublon")
        if nodes[src].kind == "end" or nodes[dst].kind == "start":
            raise DiagramError(f"flow {i + 1} : rien ne sort d'une fin ni n'entre dans un début ({src} → {dst})")
        flows.append(Flow(src, dst, str(raw.get("label", "")).strip(), i))
    if not any(n.kind == "start" for n in nodes.values()):
        raise DiagramError("au moins un nœud de kind = \"start\" est nécessaire")
    for n in nodes.values():
        if n.kind == "task":
            n.lines = wrap(n.label, TEXT_SIZE, TASK_W - 2 * TASK_PAD)
            n.w, n.h = TASK_W, max(TASK_MIN_H, line_height(TEXT_SIZE) * len(n.lines) + 2 * TASK_PAD)
        elif n.kind == "gateway":
            n.lines = wrap(n.label, TEXT_SIZE, LABEL_WIDTH) if n.label else []
            n.w = n.h = 2 * GATE_HALF
        else:
            n.lines = wrap(n.label, TEXT_SIZE, LABEL_WIDTH)
            n.w = n.h = 2 * EVENT_R
    for f in flows:
        f.lines = wrap(f.label, FLOW_SIZE, 120) if f.label else []
    return Model(title, nodes, flows)


# ── Analyse : flèches de retour, colonnes ─────────────────────────────────────────────────
@dataclass
class Graph:
    out: dict[str, list[Flow]]
    inc: dict[str, list[Flow]]  # flèches avant seulement
    order: list[Node]
    branching: list[str]
    backs: list[Flow]


def analyse(model: Model) -> Graph:
    out: dict[str, list[Flow]] = {n: [] for n in model.nodes}
    for f in model.flows:
        out[f.src].append(f)
    state: dict[str, int] = {}

    def visit(node: str) -> None:
        state[node] = 1
        for f in out[node]:
            if state.get(f.dst) == 1:
                f.back = True
            elif f.dst not in state:
                visit(f.dst)
        state[node] = 2

    for n in model.nodes.values():
        if n.kind == "start" and n.id not in state:
            visit(n.id)
    if lost := [n for n in model.nodes if n not in state]:
        raise DiagramError(f"inaccessibles depuis un début : {', '.join(lost)}")
    forward_out = {n: [f for f in fs if not f.back] for n, fs in out.items()}
    inc: dict[str, list[Flow]] = {n: [] for n in model.nodes}
    for f in model.flows:
        if not f.back:
            inc[f.dst].append(f)
    # Colonnes : plus long chemin depuis les débuts (ordre topologique de Kahn).
    pending = {n: len(fs) for n, fs in inc.items()}
    ready = sorted((model.nodes[n] for n, k in pending.items() if k == 0), key=lambda n: n.index)
    order: list[Node] = []
    while ready:
        node = ready.pop(0)
        order.append(node)
        node.col = node.col_hint if node.col_hint is not None else max((model.nodes[f.src].col + 1 for f in inc[node.id]), default=0)
        for f in forward_out[node.id]:
            pending[f.dst] -= 1
            if pending[f.dst] == 0:
                ready.append(model.nodes[f.dst])
        ready.sort(key=lambda n: n.index)
    for f in model.flows:
        if not f.back and model.nodes[f.src].col >= model.nodes[f.dst].col:
            raise DiagramError(f"{f.name} : col doit croître le long des flèches")
    order.sort(key=lambda n: (n.col, n.index))
    branching = [n for n, fs in forward_out.items() if len(fs) >= 2]
    return Graph(forward_out, inc, order, branching, [f for f in model.flows if f.back])


# ── Disposition : lignes, routage, libellés ───────────────────────────────────────────────
def hit(a: Rect, b: Rect) -> bool:
    return a[0] < b[2] and b[0] < a[2] and a[1] < b[3] and b[1] < a[3]


def grow(r: Rect, by: float) -> Rect:
    return (r[0] - by, r[1] - by, r[2] + by, r[3] + by)


def segment_box(a: Point, b: Point) -> Rect:
    return (min(a[0], b[0]), min(a[1], b[1]), max(a[0], b[0]), max(a[1], b[1]))


def touch(a: Rect, b: Rect) -> bool:
    """Croisement, contact en T ou recouvrement de deux segments (rectangles dégénérés)."""
    return a[0] <= b[2] and b[0] <= a[2] and a[1] <= b[3] and b[1] <= a[3]


def assign_rows(model: Model, graph: Graph, bits: dict[str, bool]) -> None:
    """Ligne de chaque nœud : le premier enfant reste sur la ligne du parent, les autres branches
    montent ou descendent (sens choisi par `bits`) ; une fusion revient à la ligne la plus centrale."""
    rows: dict[str, int] = {}
    depth: dict[str, int] = {}
    taken: set[tuple[int, int]] = set()

    def settle(row: int, step: int, node: Node, from_col: int) -> int:
        for _ in range(40):
            free = (node.col, row) not in taken and all((c, row) not in taken for c in range(from_col + 1, node.col))
            if free:
                return row
            row += step
        raise DiagramError(f"{node.id} : pas de place dans la grille")

    for node in graph.order:
        parents = [f.src for f in graph.inc[node.id]]
        if not parents:
            row = settle(0, 1, node, node.col - 1)
            depth[node.id] = 0
        elif len(parents) == 1:
            p = parents[0]
            k = [f.dst for f in graph.out[p]].index(node.id)
            if k == 0:
                row, step, depth[node.id] = rows[p], 1, depth[p]
            else:
                sign = 1 if bits.get(p, False) else -1
                offset = sign * ((k + 1) // 2) * (1 if k % 2 else -1)
                row, step, depth[node.id] = rows[p] + offset, (1 if offset > 0 else -1), depth[p] + 1
            row = settle(row, step, node, model.nodes[p].col)
        else:
            p = min(parents, key=lambda q: (depth[q], model.nodes[q].index))
            row, depth[node.id] = rows[p], depth[p]
            row = settle(row, 1, node, model.nodes[p].col)
        if node.row_hint is not None:
            row = node.row_hint
        rows[node.id] = row
        taken.add((node.col, row))
    lowest = min(rows.values())
    for node in model.nodes.values():
        node.cx = node.col * COL_W + COL_W / 2
        node.cy = (rows[node.id] - lowest) * ROW_H + ROW_H / 2


def route(model: Model, graph: Graph, back_below: dict[str, bool]) -> None:
    channels: dict[str, int] = {}
    lanes = {True: 0, False: 0}
    for f in model.flows:
        s, d = model.nodes[f.src], model.nodes[f.dst]
        if f.back:
            below = back_below.get(f.name, True)
            side = "bottom" if below else "top"
            span = [n for n in model.nodes.values() if d.col <= n.col <= s.col]
            edge = max(n.rect[3] for n in span) if below else min(n.rect[1] for n in span)
            lane = edge + (LANE_GAP + LANE_STEP * lanes[below]) * (1 if below else -1)
            lanes[below] += 1
            a, b = s.port(side), d.port(side)
            f.path = [a, (a[0], lane), (b[0], lane), b]
        elif s.cy == d.cy:
            f.path = [s.port("right"), d.port("left")]
        elif s.kind == "gateway":
            a = s.port("bottom" if d.cy > s.cy else "top")
            f.path = [a, (a[0], d.cy), d.port("left")]
        elif d.kind == "gateway":
            a = s.port("right")
            f.path = [a, (d.cx, s.cy), d.port("top" if s.cy < d.cy else "bottom")]
        else:
            a = s.port("right")
            x = a[0] + CHANNEL_GAP + CHANNEL_STEP * channels.get(f.src, 0)
            channels[f.src] = channels.get(f.src, 0) + 1
            f.path = [a, (x, s.cy), (x, d.cy), d.port("left")]


def label_candidates(box_w: float, box_h: float, node: Node | None, flow: Flow | None) -> list[Rect]:
    spots: list[Point] = []
    if flow is not None:
        for a, b in zip(flow.path, flow.path[1:]):
            if a[1] == b[1]:  # horizontal
                d = 1 if b[0] > a[0] else -1
                for t in (8, None, -8):  # près du départ, au milieu, près de l'arrivée
                    x = (a[0] + b[0]) / 2 - box_w / 2 if t is None else (a[0] + t * d - (box_w if d * (t or 1) < 0 else 0) if t > 0 else b[0] + t * d - (box_w if d * t < 0 else 0))
                    spots += [(x, a[1] - 5 - box_h), (x, a[1] + 5)]
            else:  # vertical
                d = 1 if b[1] > a[1] else -1
                for t in (8, None, -8):
                    y = (a[1] + b[1]) / 2 - box_h / 2 if t is None else (a[1] + t * d - (box_h if d * (t or 1) < 0 else 0) if t > 0 else b[1] + t * d - (box_h if d * t < 0 else 0))
                    spots += [(a[0] + 6, y), (a[0] - 6 - box_w, y)]
    elif node is not None:
        cx, cy, half = node.cx, node.cy, node.h / 2
        if node.kind == "gateway":
            for extra in (0, 16, 32):
                spots += [(cx - 6 - box_w, cy - half - 2 - box_h - extra), (cx + 6, cy - half - 2 - box_h - extra),
                          (cx - 6 - box_w, cy + half + 2 + extra), (cx + 6, cy + half + 2 + extra)]
        else:
            for extra in (0, 16, 32):
                spots += [(cx - box_w / 2, cy + half + 6 + extra), (cx - box_w / 2, cy - half - 6 - box_h - extra)]
            spots += [(cx + half + 8, cy - box_h / 2), (cx - half - 8 - box_w, cy - box_h / 2)]
    return [(x, y, x + box_w, y + box_h) for x, y in spots]


def place_labels(model: Model, graph: Graph) -> tuple[dict[str, Rect], list[str]]:
    problems: list[str] = []
    placed: list[Rect] = []
    node_labels: dict[str, Rect] = {}
    segments = [(f, segment_box(a, b)) for f in model.flows for a, b in zip(f.path, f.path[1:])]
    nodes = {n.id: n.rect for n in model.nodes.values()}

    def free(rect: Rect, own_node: str | None) -> bool:
        return (not any(hit(rect, grow(r, 4)) for i, r in nodes.items() if i != own_node)
                and not any(hit(rect, grow(r, 3)) for r in placed)
                and not any(hit(rect, grow(box, 2)) for _, box in segments))

    for n in model.nodes.values():
        if n.kind == "task" or not n.lines:
            continue
        w, h = text_box(n.lines, TEXT_SIZE)
        if choice := next((r for r in label_candidates(w, h, n, None) if free(r, n.id)), None):
            node_labels[n.id] = choice
            placed.append(choice)
        else:
            problems.append(f"pas de place pour le libellé de {n.id}")
    for f in model.flows:
        f.label_rect = None
        if not f.lines:
            continue
        w, h = text_box(f.lines, FLOW_SIZE)
        if choice := next((r for r in label_candidates(w, h, None, f) if free(r, None)), None):
            f.label_rect = choice
            placed.append(choice)
        else:
            problems.append(f"pas de place pour le libellé de {f.name} ({f.label!r})")
    return node_labels, problems


def check_geometry(model: Model) -> list[str]:
    problems: list[str] = []
    flows = model.flows
    for i, f in enumerate(flows):
        boxes = [segment_box(a, b) for a, b in zip(f.path, f.path[1:])]
        for n in model.nodes.values():
            if n.id not in (f.src, f.dst) and any(hit(box, grow(n.rect, -1)) or (box[0] == box[2] and hit(grow(box, 0.5), grow(n.rect, -1))) for box in boxes):
                problems.append(f"{f.name} traverse {n.id}")
        for g in flows[i + 1:]:
            if {f.src, f.dst} & {g.src, g.dst}:
                continue
            if any(touch(a, segment_box(c, d)) for a, b in zip(f.path, f.path[1:]) for c, d in zip(g.path, g.path[1:]) for a in [segment_box(a, b)]):
                problems.append(f"{f.name} croise {g.name}")
    return problems


@dataclass
class Layout:
    node_labels: dict[str, Rect]
    problems: list[str]


def arrange(model: Model, graph: Graph) -> Layout:
    """Essaie les dispositions (sens des branches, côté des flèches de retour) jusqu'à en trouver une
    sans défaut ; sinon garde la moins mauvaise."""
    switches = [("branch", n) for n in graph.branching] + [("back", f.name) for f in graph.backs]
    best: Layout | None = None
    for mask in range(min(2 ** len(switches), MAX_COMBOS)):
        chosen = {name: bool(mask >> i & 1) for i, (_, name) in enumerate(switches)}
        assign_rows(model, graph, {n: chosen[n] for kind, n in switches if kind == "branch"})
        route(model, graph, {name: not chosen[name] for kind, name in switches if kind == "back"})
        labels, label_problems = place_labels(model, graph)
        layout = Layout(labels, check_geometry(model) + label_problems)
        if not layout.problems:
            return layout
        if best is None or len(layout.problems) < len(best.problems):
            best, best_choice = layout, chosen
    assert best is not None
    assign_rows(model, graph, {n: best_choice[n] for kind, n in switches if kind == "branch"})
    route(model, graph, {name: not best_choice[name] for kind, name in switches if kind == "back"})
    labels, label_problems = place_labels(model, graph)
    return Layout(labels, check_geometry(model) + label_problems)


# ── Dessin ─────────────────────────────────────────────────────────────────────────────────
def render(model: Model, layout: Layout) -> Image.Image:
    rects = [n.rect for n in model.nodes.values()] + list(layout.node_labels.values())
    rects += [f.label_rect for f in model.flows if f.label_rect]
    rects += [segment_box(a, b) for f in model.flows for a, b in zip(f.path, f.path[1:])]
    min_x, min_y = min(r[0] for r in rects), min(r[1] for r in rects)
    max_x, max_y = max(r[2] for r in rects), max(r[3] for r in rects)
    title_w = width(model.title, TITLE_SIZE, True)
    full_w = max(max_x - min_x, title_w) + 2 * MARGIN
    full_h = TITLE_H + (max_y - min_y) + MARGIN
    ox, oy = MARGIN - min_x, TITLE_H - min_y

    image = Image.new("RGB", (round(full_w * FACTOR), round(full_h * FACTOR)), BACKGROUND)
    draw = ImageDraw.Draw(image)

    def at(p: Point) -> Point:
        return ((p[0] + ox) * FACTOR, (p[1] + oy) * FACTOR)

    def box(r: Rect) -> list[float]:
        return [*at((r[0], r[1])), *at((r[2], r[3]))]

    def text(lines: list[str], r: Rect, size: float, center: bool, bold: bool = False) -> None:
        for i, line in enumerate(lines):
            x = (r[0] + r[2]) / 2 if center else r[0]
            draw.text(at((x, r[1] + i * line_height(size) + line_height(size) / 2)), line, font=font(size, bold),
                      fill=INK, anchor="mm" if center else "lm")

    draw.text((MARGIN * FACTOR, 30 * FACTOR), model.title, font=font(TITLE_SIZE, True), fill=INK, anchor="lt")
    rule = TITLE_H - 16
    draw.line([(MARGIN * FACTOR, rule * FACTOR), ((full_w - MARGIN) * FACTOR, rule * FACTOR)], fill="#cbd2d9", width=FACTOR)

    for f in model.flows:
        points = [at(p) for p in f.path]
        (x0, y0), (x1, y1) = f.path[-2], f.path[-1]
        length = abs(x1 - x0) + abs(y1 - y0)
        ux, uy = (x1 - x0) / length, (y1 - y0) / length
        head, spread = 11, 5
        base = (x1 - ux * head, y1 - uy * head)
        points[-1] = at(base)
        draw.line(points, fill=FLOW_INK, width=round(1.6 * FACTOR), joint="curve")
        draw.polygon([at((x1, y1)), at((base[0] - uy * spread, base[1] + ux * spread)), at((base[0] + uy * spread, base[1] - ux * spread))], fill=FLOW_INK)
        if f.label_rect:
            text(f.lines, f.label_rect, FLOW_SIZE, False)

    for n in model.nodes.values():
        fill, outline, outline_w = STYLE[n.kind]
        r = n.rect
        if n.kind == "task":
            draw.rounded_rectangle(box(r), radius=10 * FACTOR, fill=fill, outline=outline, width=outline_w * FACTOR // 2)
            text(n.lines, (r[0] + TASK_PAD, n.cy - line_height(TEXT_SIZE) * len(n.lines) / 2, r[2] - TASK_PAD, r[3]), TEXT_SIZE, True)
        elif n.kind == "gateway":
            draw.polygon([at(n.port(s)) for s in ("top", "right", "bottom", "left")], fill=fill, outline=outline, width=outline_w * FACTOR // 2)
            k = 9
            if n.marker == "parallel":
                draw.line([at((n.cx - k, n.cy)), at((n.cx + k, n.cy))], fill=outline, width=3 * FACTOR // 2)
                draw.line([at((n.cx, n.cy - k)), at((n.cx, n.cy + k))], fill=outline, width=3 * FACTOR // 2)
            else:
                k = 7
                draw.line([at((n.cx - k, n.cy - k)), at((n.cx + k, n.cy + k))], fill=outline, width=3 * FACTOR // 2)
                draw.line([at((n.cx - k, n.cy + k)), at((n.cx + k, n.cy - k))], fill=outline, width=3 * FACTOR // 2)
        else:
            draw.ellipse(box(r), fill=fill, outline=outline, width=outline_w * FACTOR // 2)
        if label := layout.node_labels.get(n.id):
            text(n.lines, label, TEXT_SIZE, True)

    return image.resize((round(full_w * OUTPUT_SCALE), round(full_h * OUTPUT_SCALE)), Image.LANCZOS)


# ── Programme ──────────────────────────────────────────────────────────────────────────────
def generate(source: Path, out_dir: Path, force: bool) -> Path:
    model = load(source)
    graph = analyse(model)
    layout = arrange(model, graph)
    if layout.problems and not force:
        raise DiagramError("impossible sans défaut de lisibilité :\n  - " + "\n  - ".join(layout.problems)
                           + "\nAjoutez `row` ou `col` à des nœuds, ou simplifiez le flux (--force pour dessiner quand même).")
    target = out_dir / f"{source.stem}.png"
    render(model, layout).save(target, optimize=True)
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description="Génère des PNG de diagrammes BPMN à partir de fichiers TOML.")
    parser.add_argument("files", nargs="*", type=Path, help="fichiers .toml (par défaut : ceux du dossier du script)")
    parser.add_argument("--out", type=Path, help="dossier des PNG (par défaut : à côté de chaque source)")
    parser.add_argument("--force", action="store_true", help="dessine même si un défaut de lisibilité reste")
    args = parser.parse_args()
    sources = args.files or sorted(Path(__file__).resolve().parent.glob("*.toml"))
    if not sources:
        print("aucun fichier .toml", file=sys.stderr)
        return 1
    failed = 0
    for source in sources:
        try:
            target = generate(source, args.out or source.resolve().parent, args.force)
            print(f"{source} → {target}")
        except DiagramError as error:
            failed += 1
            print(f"{source}: {error}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
