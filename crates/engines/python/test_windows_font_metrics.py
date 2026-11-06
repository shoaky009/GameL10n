"""Font comparisons and native Windows GDI/GDI+ font-creation checks."""
import ctypes
import importlib.util
import os
from pathlib import Path
import unittest


COMPONENT = Path(__file__).resolve().parents[3] / "adapters/components/windows_font_metrics.py"
SPEC = importlib.util.spec_from_file_location("windows_font_metrics", COMPONENT)
METRICS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(METRICS)


class FontComparisonTest(unittest.TestCase):
    def test_missing_glyph_fallback_and_blank_boxes_do_not_distort_ink_ratios(self):
        def font(height):
            return dict(textMetrics=dict(emHeight=height, ascent=height, height=height))

        original = [dict(character="A", present=True, measuredAdvance=20, inkWidth=16, inkHeight=24),
                    dict(character=" ", present=True, measuredAdvance=10, inkWidth=1, inkHeight=1),
                    dict(character="中", present=False, measuredAdvance=999)]
        candidate = [dict(character="A", present=True, measuredAdvance=10, inkWidth=8, inkHeight=12),
                     dict(character=" ", present=True, measuredAdvance=5, inkWidth=20, inkHeight=20),
                     dict(character="中", present=True, measuredAdvance=1, inkWidth=1, inkHeight=1)]
        result = METRICS.compare_fonts((font(30), original), (font(15), candidate), 1)
        self.assertEqual(result["sharedGlyphCount"], 2)
        self.assertEqual(result["sharedOutlineGlyphCount"], 1)
        for key in ("advanceRatio", "inkWidthRatio", "inkHeightRatio"):
            self.assertEqual(result[key], 0.5)

    def test_no_shared_coverage_keeps_ratios_unknown(self):
        font = dict(textMetrics=dict(emHeight=24, ascent=24, height=26))
        original = [dict(character="A", present=True, measuredAdvance=12)]
        candidate = [dict(character="A", present=False, measuredAdvance=24),
                     dict(character="中", present=True, measuredAdvance=24)]
        result = METRICS.compare_fonts((font, original), (font, candidate), 1)
        self.assertEqual(result["sharedGlyphCount"], 0)
        for key in ("advanceRatio", "inkWidthRatio", "inkHeightRatio"):
            self.assertIsNone(result[key])


class GdiplusProbeTest(unittest.TestCase):
    def test_private_collection_success_keeps_dc_and_ansi_failures_visible(self):
        deleted = []

        def create(pointer, value):
            ctypes.cast(pointer, ctypes.POINTER(ctypes.c_void_p))[0] = value
            return 0

        class Gdi:
            def GetObjectA(self, font, size, logical_font):
                return size

            GetObjectW = GetObjectA

        class Gdiplus:
            def GdipCreateFontFromDC(self, dc, pointer):
                return 16

            def GdipCreateFontFromLogfontA(self, dc, logical_font, pointer):
                return 16

            def GdipCreateFontFromLogfontW(self, dc, logical_font, pointer):
                return create(pointer, 102)

            def GdipCreateFontFamilyFromName(self, name, collection, pointer):
                return create(pointer, 103) if collection else 14

            def GdipCreateFont(self, family, em_size, style, unit, pointer):
                return create(pointer, 104)

            def GdipNewPrivateFontCollection(self, pointer):
                return create(pointer, 101)

            def GdipPrivateAddFontFile(self, collection, path):
                return 0

            def GdipDeleteFont(self, font):
                deleted.append(("font", font.value))

            def GdipDeleteFontFamily(self, family):
                deleted.append(("family", family.value))

            def GdipDeletePrivateFontCollection(self, pointer):
                deleted.append(("collection", ctypes.cast(pointer, ctypes.POINTER(ctypes.c_void_p))[0]))

        lf = METRICS.LOGFONT()
        lf.faceName = "Private Test Font"
        lf.weight = 400
        result = METRICS.inspect_gdiplus(Gdiplus(), Gdi(), 1, 2, lf, 24, "test.otf")
        for key in ("fromDC", "fromLogfontA"):
            self.assertEqual(result[key], dict(status=16, statusName="NotTrueTypeFont", created=False))
        self.assertTrue(result["fromLogfontW"]["created"])
        self.assertEqual(result["byName"]["family"]["status"], 14)
        self.assertFalse(result["byName"]["font"]["attempted"])
        self.assertTrue(result["privateCollection"]["family"]["created"])
        self.assertTrue(result["privateCollection"]["font"]["created"])
        self.assertEqual(deleted, [("font", 102), ("font", 104), ("family", 103), ("collection", 101)])

    def test_font_and_family_are_released_when_font_creation_raises(self):
        deleted = []

        class Gdi:
            def GetObjectA(self, font, size, logical_font):
                return size

            GetObjectW = GetObjectA

        class Gdiplus:
            def GdipCreateFontFromDC(self, dc, pointer):
                return 16

            GdipCreateFontFromLogfontA = lambda self, dc, lf, pointer: 16
            GdipCreateFontFromLogfontW = GdipCreateFontFromLogfontA

            def GdipCreateFontFamilyFromName(self, name, collection, pointer):
                ctypes.cast(pointer, ctypes.POINTER(ctypes.c_void_p))[0] = 103
                return 0

            def GdipCreateFont(self, family, em_size, style, unit, pointer):
                ctypes.cast(pointer, ctypes.POINTER(ctypes.c_void_p))[0] = 104
                raise RuntimeError("Interrupted font creation")

            def GdipDeleteFont(self, font):
                deleted.append(("font", font.value))

            def GdipDeleteFontFamily(self, family):
                deleted.append(("family", family.value))

        lf = METRICS.LOGFONT()
        lf.faceName = "Test Font"
        with self.assertRaisesRegex(RuntimeError, "Interrupted font creation"):
            METRICS.inspect_gdiplus(Gdiplus(), Gdi(), 1, 2, lf, 24, None)
        self.assertEqual(deleted, [("font", 104), ("family", 103)])


@unittest.skipUnless(os.name == "nt", "Requires Windows GDI")
class WindowsFontMetricsTest(unittest.TestCase):
    def test_signed_height_and_baseline_metrics_use_real_gdi_values(self):
        result = METRICS.execute(dict(fonts=[dict(face="Arial", height=40),
                                            dict(face="Arial", height=-40)], text="Hg"))
        self.assertEqual(ctypes.sizeof(METRICS.TEXTMETRIC), 60)
        positive, negative = result["fonts"]
        self.assertNotIn("gdiplus", positive)
        self.assertEqual(negative["textMetrics"]["emHeight"], 40)
        self.assertLess(positive["textMetrics"]["emHeight"], negative["textMetrics"]["emHeight"])
        for font in result["fonts"]:
            tm = font["textMetrics"]
            self.assertEqual(tm["height"], tm["ascent"] + tm["descent"])
            self.assertEqual(tm["emHeight"], tm["height"] - tm["internalLeading"])
            glyphs = {g["character"]: g for g in font["samples"]}
            self.assertLess(glyphs["H"]["inkTop"], 0)
            self.assertGreater(glyphs["g"]["inkBottom"], 0)
            for glyph in glyphs.values():
                self.assertEqual(glyph["inkBottom"] - glyph["inkTop"], glyph["inkHeight"])

    def test_smaller_font_is_detected_with_missing_and_non_bmp_text_excluded(self):
        result = METRICS.execute(dict(fonts=[dict(face="Arial", height=40),
                                            dict(face="Arial", height=20)], text="HgWy09\u0378😀"))
        for font in result["fonts"]:
            self.assertFalse(font["coversSample"])
            self.assertEqual(font["missingCount"], 1)
            self.assertEqual(font["unsupportedCount"], 1)
        comparison = result["comparisons"][0]
        self.assertEqual(comparison["sharedGlyphCount"], 6)
        self.assertEqual(comparison["sharedOutlineGlyphCount"], 6)
        for key in ("advanceRatio", "inkWidthRatio", "inkHeightRatio"):
            self.assertGreater(comparison[key], 0.4)
            self.assertLess(comparison[key], 0.6)

    def test_installed_font_can_be_created_from_dc_and_both_logfont_forms(self):
        result = METRICS.execute(dict(renderer="gdiplus", fonts=[dict(face="Arial", height=-24)], text="Hg"))
        self.assertEqual(ctypes.sizeof(METRICS.LOGFONT), 92)
        self.assertEqual(ctypes.sizeof(METRICS.LOGFONTA), 60)
        self.assertEqual(result["renderer"], "gdiplus")
        probe = result["fonts"][0]["gdiplus"]
        for key in ("fromDC", "fromLogfontA", "fromLogfontW"):
            self.assertEqual(probe[key], dict(status=0, statusName="Ok", created=True))
        self.assertTrue(probe["byName"]["family"]["created"])
        self.assertTrue(probe["byName"]["font"]["created"])
        self.assertFalse(probe["privateCollection"]["attempted"])

    def test_supplied_font_also_uses_an_explicit_private_collection(self):
        font_file = Path(os.environ.get("WINDIR", r"C:\Windows")) / "Fonts/arial.ttf"
        if not font_file.is_file():
            self.skipTest("Arial font file is unavailable")
        result = METRICS.execute(dict(renderer="gdiplus", fonts=[dict(face="Arial", file=str(font_file))], text="Hg"))
        probe = result["fonts"][0]["gdiplus"]
        self.assertTrue(probe["fromDC"]["created"])
        for key in ("createCollection", "family", "font"):
            self.assertEqual(probe["privateCollection"][key], dict(status=0, statusName="Ok", created=True))
        self.assertEqual(probe["privateCollection"]["addFontFile"], dict(status=0, statusName="Ok"))

    def test_unknown_renderer_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "renderer must be gdi or gdiplus"):
            METRICS.execute(dict(renderer="other", fonts=[dict(face="Arial")], text="Hg"))


if __name__ == "__main__":
    unittest.main()
