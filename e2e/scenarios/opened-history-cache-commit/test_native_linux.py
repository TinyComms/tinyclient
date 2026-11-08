import unittest

import native_linux


def word(text, x, y):
    return {"text": text, "x": x, "y": y, "width": 60, "height": 18}


def intro():
    return [word(text, 420 + index * 65, 160)
            for index, text in enumerate("This is the start of your conversation with Bea".split())]


class LinuxPaneTests(unittest.TestCase):
    def test_sidebar_preview_cannot_certify_the_conversation(self):
        words = [word("opened-history-cache-commit-rose", 25, 70),
                 *intro(), word("Can't", 420, 230), word("decrypt", 490, 230)]
        self.assertIs(native_linux.conversation_marker(words, "opened-history-cache-commit-rose"), False)

    def test_body_marker_survives_ocr_word_and_line_splits(self):
        words = [*intro(), word("opened-history-", 420, 220),
                 word("cache-commit-rose", 420, 245)]
        self.assertIs(native_linux.conversation_marker(words, "opened-history-cache-commit-rose"), True)

    def test_partial_body_marker_does_not_pass(self):
        words = [*intro(), word("opened-history-cache-commit", 420, 220)]
        self.assertIs(native_linux.conversation_marker(words, "opened-history-cache-commit-rose"), False)

    def test_missing_conversation_intro_is_setup_incomplete(self):
        self.assertIsNone(native_linux.conversation_marker(
            [word("opened-history-cache-commit-rose", 25, 70)], "opened-history-cache-commit-rose"))

    def test_welcome_this_computer_text_is_not_a_conversation_intro(self):
        words = [word("Link", 400, 513), word("this", 427, 513), word("computer", 460, 513),
                 word("Your", 350, 701), word("sign-in", 390, 701), word("this", 646, 701)]
        self.assertIsNone(native_linux.conversation_marker(words, "opened-history-cache-commit-rose"))


if __name__ == "__main__":
    unittest.main()
