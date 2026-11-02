"""Tests du générateur : python3 -m unittest discover -s docs/bpmn"""

import tempfile
import unittest
from pathlib import Path

import bpmn

HERE = Path(__file__).resolve().parent

SIMPLE = """
title = "Simple"
[[node]]
id = "a"
kind = "start"
label = "Début"
[[node]]
id = "b"
label = "Faire"
[[node]]
id = "c"
kind = "end"
label = "Fin"
[[flow]]
from = "a"
to = "b"
[[flow]]
from = "b"
to = "c"
"""

BRANCH = """
title = "Branches"
[[node]]
id = "s"
kind = "start"
label = "Début"
[[node]]
id = "g"
kind = "gateway"
label = "Choix ?"
[[node]]
id = "x"
label = "Voie X"
[[node]]
id = "y"
label = "Voie Y"
[[node]]
id = "e"
kind = "end"
label = "Fin"
[[flow]]
from = "s"
to = "g"
[[flow]]
from = "g"
to = "x"
label = "Oui"
[[flow]]
from = "g"
to = "y"
label = "Non"
[[flow]]
from = "x"
to = "e"
[[flow]]
from = "y"
to = "e"
"""

# Deux flèches qui doivent se croiser : x vers b, y vers a, lignes imposées à l'envers.
CROSSING = """
title = "Croisement"
[[node]]
id = "s"
kind = "start"
label = "Début"
[[node]]
id = "g"
kind = "gateway"
label = ""
[[node]]
id = "a"
label = "A"
col = 3
row = 0
[[node]]
id = "b"
label = "B"
col = 3
row = 1
[[node]]
id = "x"
label = "X"
col = 2
row = 0
[[node]]
id = "y"
label = "Y"
col = 2
row = 1
[[flow]]
from = "s"
to = "g"
[[flow]]
from = "g"
to = "x"
[[flow]]
from = "g"
to = "y"
[[flow]]
from = "x"
to = "b"
[[flow]]
from = "y"
to = "a"
"""


class Case(unittest.TestCase):
    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        self.dir = Path(self._dir.name)

    def tearDown(self):
        self._dir.cleanup()

    def source(self, text: str, name: str = "d.toml") -> Path:
        path = self.dir / name
        path.write_text(text, encoding="utf-8")
        return path

    def arranged(self, text: str):
        model = bpmn.load(self.source(text))
        graph = bpmn.analyse(model)
        return model, bpmn.arrange(model, graph)


class Definition(Case):
    def test_title_is_required(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "title"):
            bpmn.load(self.source(SIMPLE.replace('title = "Simple"', "")))

    def test_blank_title_is_refused(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "title"):
            bpmn.load(self.source(SIMPLE.replace('"Simple"', '"  "')))

    def test_unknown_node_in_flow(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "inconnu"):
            bpmn.load(self.source(SIMPLE + '[[flow]]\nfrom = "c"\nto = "zzz"\n'))

    def test_nothing_leaves_an_end(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "fin"):
            bpmn.load(self.source(SIMPLE + '[[flow]]\nfrom = "c"\nto = "b"\n'))

    def test_duplicate_id(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "double"):
            bpmn.load(self.source(SIMPLE + '[[node]]\nid = "a"\nlabel = "x"\n'))

    def test_unknown_key_and_kind(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "inconnue"):
            bpmn.load(self.source(SIMPLE + "colour = 1\n"))
        with self.assertRaisesRegex(bpmn.DiagramError, "kind"):
            bpmn.load(self.source(SIMPLE.replace('kind = "end"', 'kind = "blob"')))

    def test_a_start_is_needed(self):
        with self.assertRaisesRegex(bpmn.DiagramError, "start"):
            bpmn.load(self.source(SIMPLE.replace('kind = "start"', 'kind = "task"')))

    def test_unreachable_node(self):
        model = bpmn.load(self.source(SIMPLE + '[[node]]\nid = "lost"\nlabel = "Perdu"\n'))
        with self.assertRaisesRegex(bpmn.DiagramError, "lost"):
            bpmn.analyse(model)


class Layout(Case):
    def test_simple_line_has_no_defect(self):
        model, layout = self.arranged(SIMPLE)
        self.assertEqual(layout.problems, [])
        self.assertEqual({n.cy for n in model.nodes.values()}, {bpmn.ROW_H / 2})

    def test_branches_go_to_different_rows(self):
        model, layout = self.arranged(BRANCH)
        self.assertEqual(layout.problems, [])
        self.assertNotEqual(model.nodes["x"].cy, model.nodes["y"].cy)

    def test_loop_is_a_back_edge_and_stays_clean(self):
        text = SIMPLE.replace("[[flow]]\nfrom = \"b\"\nto = \"c\"", "[[node]]\nid = \"g\"\nkind = \"gateway\"\nlabel = \"Encore ?\"\n"
                              "[[flow]]\nfrom = \"b\"\nto = \"g\"\n[[flow]]\nfrom = \"g\"\nto = \"b\"\nlabel = \"Oui\"\n"
                              "[[flow]]\nfrom = \"g\"\nto = \"c\"\nlabel = \"Non\"")
        model, layout = self.arranged(text)
        self.assertEqual([f.name for f in model.flows if f.back], ["g → b"])
        self.assertEqual(layout.problems, [])

    def test_no_label_overlaps_anything(self):
        model, layout = self.arranged(BRANCH)
        rects = list(layout.node_labels.values()) + [f.label_rect for f in model.flows if f.label_rect]
        for i, a in enumerate(rects):
            for b in rects[i + 1:]:
                self.assertFalse(bpmn.hit(a, b))
            for n in model.nodes.values():
                self.assertFalse(bpmn.hit(a, n.rect))

    def test_a_forced_crossing_is_reported(self):
        text = CROSSING
        _, layout = self.arranged(text)
        self.assertTrue(any("croise" in p or "traverse" in p for p in layout.problems), layout.problems)

    def test_generate_refuses_defects_without_force(self):
        path = self.source(CROSSING)
        with self.assertRaisesRegex(bpmn.DiagramError, "croise|traverse"):
            bpmn.generate(path, self.dir, force=False)
        self.assertFalse((self.dir / "d.png").exists())
        bpmn.generate(path, self.dir, force=True)
        self.assertTrue((self.dir / "d.png").exists())


class Output(Case):
    def test_png_is_written_with_a_title_area(self):
        target = bpmn.generate(self.source(SIMPLE), self.dir, force=False)
        self.assertEqual(target.name, "d.png")
        from PIL import Image

        with Image.open(target) as image:
            self.assertGreater(image.width, 400)
            # Le haut de l'image porte le titre : au moins un pixel non blanc dans la bande du titre.
            band = image.convert("L").crop((0, 0, image.width, bpmn.TITLE_H * bpmn.OUTPUT_SCALE - 10))
            self.assertLess(band.getextrema()[0], 128)

    def test_committed_diagrams_are_clean_and_up_to_date(self):
        for source in sorted(HERE.glob("*.toml")):
            with self.subTest(source=source.name):
                bpmn.generate(source, self.dir, force=False)
                self.assertTrue(source.with_suffix(".png").exists(), f"lancez make bpmn : {source.stem}.png manque")


if __name__ == "__main__":
    unittest.main()
