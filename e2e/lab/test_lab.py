import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import common  # noqa: E402


def clip(path, source, seconds=8, size="720x1280"):
    subprocess.run([common.tool("ffmpeg"), "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                    "%s=s=%s:d=%d:r=10" % (source, size, seconds) if source == "testsrc2" else "%s:s=%s:d=%d:r=10" % (source, size, seconds),
                    "-c:v", "libx264", "-pix_fmt", "yuv420p", path], check=True)
    return path


STEPS = [{"t": 0.5, "step": "launch", "ok": True}, {"t": 5.0, "step": "inbox shown", "ok": True}]


class Validate(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def path(self, name):
        return os.path.join(self.dir.name, name)

    def test_moving_picture_passes(self):
        self.assertEqual(common.validate(clip(self.path("a.mp4"), "testsrc2"), 8, STEPS), [])

    def test_black_picture_fails(self):
        problems = common.validate(clip(self.path("b.mp4"), "color=c=black"), 8, STEPS)
        self.assertTrue(any("black" in p for p in problems), problems)

    def test_frozen_picture_fails(self):
        problems = common.validate(clip(self.path("f.mp4"), "color=c=0x336699"), 8, STEPS)
        self.assertTrue(any("never changes" in p or "frozen" in p for p in problems), problems)

    def test_steps_off_camera_fail(self):
        steps = STEPS + [{"t": 40.0, "step": "after the recording", "ok": True}]
        problems = common.validate(clip(self.path("c.mp4"), "testsrc2"), 8, steps)
        self.assertTrue(any("off camera" in p for p in problems), problems)

    def test_no_steps_fail(self):
        problems = common.validate(clip(self.path("d.mp4"), "testsrc2"), 8, [])
        self.assertTrue(any("no steps" in p for p in problems), problems)

    def test_small_picture_fails(self):
        problems = common.validate(clip(self.path("e.mp4"), "testsrc2", size="320x240"), 8, STEPS)
        self.assertTrue(any("short side" in p for p in problems), problems)

    def test_missing_and_garbage_files_fail(self):
        self.assertEqual(common.validate("", 1, STEPS), ["there is no recording"])
        junk = self.path("junk.mp4")
        open(junk, "wb").write(b"not a video")
        self.assertEqual(common.validate(junk, 1, STEPS), ["the recording does not play"])

    def test_run_shorter_than_recording_is_caught(self):
        problems = common.validate(clip(self.path("g.mp4"), "testsrc2", seconds=4), 30, STEPS)
        self.assertTrue(any("for a 30.0s run" in p for p in problems), problems)

    def test_a_failing_run_may_sit_still(self):
        still = clip(self.path("h.mp4"), "color=c=0x336699")
        self.assertEqual(common.validate(still, 8, STEPS, expect_pass=False), [])
        self.assertNotEqual(common.validate(still, 8, STEPS, expect_pass=True), [])

    def test_a_failing_run_still_has_to_show_its_steps(self):
        problems = common.validate(self.path("none.mp4"), 8, STEPS, expect_pass=False)
        self.assertEqual(problems, ["there is no recording"])

    def test_variable_frame_rate_recorders_end_at_the_last_change(self):
        short = clip(self.path("v.mp4"), "testsrc2", seconds=4)
        steps = [{"t": 1, "step": "a", "ok": True}, {"t": 15, "step": "b", "ok": True}]
        self.assertEqual(common.validate(short, 16, steps, vfr=True), [])
        self.assertNotEqual(common.validate(short, 16, steps, vfr=False), [])

    def test_vfr_does_not_excuse_steps_after_the_run_ended(self):
        short = clip(self.path("w.mp4"), "testsrc2", seconds=4)
        steps = [{"t": 1, "step": "a", "ok": True}, {"t": 60, "step": "b", "ok": True}]
        self.assertTrue(any("off camera" in p for p in common.validate(short, 16, steps, vfr=True)))

    def test_a_mostly_still_ui_recording_is_fine(self):
        # a screen that holds for 6 s, then changes: how a real app recording looks
        still = self.path("s1.mp4")
        move = self.path("s2.mp4")
        both = self.path("s3.mp4")
        clip(still, "color=c=0x336699", seconds=6)
        clip(move, "testsrc2", seconds=3)
        listing = self.path("list.txt")
        with open(listing, "w") as f:
            f.write("file '%s'\nfile '%s'\n" % (still.replace("\\", "/"), move.replace("\\", "/")))
        subprocess.run([common.tool("ffmpeg"), "-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0",
                        "-i", listing, "-c", "copy", both], check=True)
        self.assertEqual(common.validate(both, 9, STEPS), [])


class Caption(unittest.TestCase):
    def test_caption_keeps_the_picture_and_pads_to_the_run(self):
        with tempfile.TemporaryDirectory() as d:
            raw = clip(os.path.join(d, "raw.mp4"), "testsrc2", seconds=4)
            out = common.caption(raw, os.path.join(d, "out.mp4"), "tinyclient@abc1234 ios branch", STEPS, 7.0)
            info = common.probe(out)
            self.assertGreaterEqual(info["seconds"], 6.5)
            self.assertEqual((info["width"], info["height"]), (720, 1280))
            self.assertEqual(common.validate(out, 7.0, STEPS), [])

    def test_awkward_step_text_does_not_break_the_filter(self):
        with tempfile.TemporaryDirectory() as d:
            raw = clip(os.path.join(d, "raw.mp4"), "testsrc2", seconds=4)
            steps = [{"t": 0.2, "step": "it's 50% done: [a, b] \\ c", "ok": False}]
            out = common.caption(raw, os.path.join(d, "out.mp4"), "x@y: z", steps, 4.0)
            self.assertIsNotNone(common.probe(out))


class Steps(unittest.TestCase):
    def test_reads_lines_and_skips_blanks(self):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write('{"t": 1, "step": "a"}\n\n{"t": 2, "step": "b"}\n')
        self.addCleanup(os.remove, f.name)
        self.assertEqual([s["step"] for s in common.read_steps(f.name)], ["a", "b"])
        self.assertEqual(common.read_steps("/no/such/file"), [])


if __name__ == "__main__":
    unittest.main()
