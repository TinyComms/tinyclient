#!/usr/bin/env python3
"""Drive the real desktop app against the Wave 11 Discord fixture."""
import pathlib
import runpy
import sys


helpers = runpy.run_path(str(pathlib.Path(__file__).resolve().parent.parent /
                            'discord-profile-add-friend' / 'drive.py'))
expect, find, tap = (helpers[name] for name in ('expect', 'find', 'tap'))


def press(name, words, button='left', required=()):
    path, lines = expect(name, [words, *required])
    point = find(lines, words)
    if button == 'right':
        import pyautogui
        pyautogui.click(*point, button='right')
    else:
        tap(point, path)


def main():
    import pyautogui

    press('01-poll-and-bot-embeds-visible', 'Studio', required=['Where next?', 'Build 812', 'Coverage'])
    expect('02-vote-updates-the-poll', ['Where next?', 'Studio', 'View voters'])
    press('03-voter-action', 'View voters')
    expect('04-voters-are-named', ['Voters', 'Wave Voter'])
    press('05-retract-vote', 'Studio')
    expect('06-vote-is-cleared', ['Where next?', 'Show results'])

    _, lines = expect('07-image-description-badge', ['ALT'])
    pyautogui.moveTo(*find(lines, 'ALT'))
    expect('08-image-description', ['A lamp over a drafting table'])

    press('09-voice-status-visible', 'Wave Lounge', 'right', ['Studying'])
    press('10-status-editor', 'Set channel status')
    path, lines = expect('11-existing-status-in-editor', ['Set channel status', 'Studying', 'Save'])
    tap(find(lines, 'Studying'), path)
    pyautogui.hotkey('command' if sys.argv[1] == 'macos' else 'ctrl', 'a')
    pyautogui.write('Ready for a review')
    press('12-save-voice-status', 'Save')
    expect('13-updated-status-in-tree', ['Wave Lounge', 'Ready for a review'])

    press('14-open-voice-channel-chat', 'Wave Lounge')
    press('15-open-voice-settings', 'Voice settings')
    expect('16-voice-settings', ['Bitrate', 'User limit', 'Video quality', 'Automatic'])
    press('17-cancel-unchanged-settings', 'Cancel')
    press('18-voice-occupant-menu', 'Wave Guest', 'right')
    expect('19-moderation-actions', ['Server mute', 'Server deafen', 'Move to', 'Disconnect from call'])
    press('20-server-mute-the-guest', 'Server mute')
    press('21-verify-gateway-mute', 'Wave Guest', 'right')
    expect('22-mute-is-reflected', ['Server unmute'])
    pyautogui.press('esc')

    pyautogui.hotkey('command' if sys.argv[1] == 'macos' else 'ctrl', 'k')
    pyautogui.write('Wave DM')
    expect('23-find-the-fixture-dm', ['Wave DM'])
    pyautogui.press('enter')
    press('24-message-context-menu', 'Wave pin this message', 'right')
    press('25-pin-in-dm', 'Pin')
    press('26-reopen-the-message-menu', 'Wave pin this message', 'right')
    expect('27-dm-pin-succeeded', ['Unpin'])


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(error, file=sys.stderr)
        sys.exit(1)
