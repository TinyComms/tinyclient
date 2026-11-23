"""Drive the application's AT-SPI tree and keyboard, as Orca can."""
import json
import os
import re
import sys
import time
from pathlib import Path

import pyatspi

PID = int(sys.argv[1])
T0 = float(os.environ["E2E_T0"])
STEPS = Path(os.environ["E2E_OUT"]) / "steps.jsonl"


def step(name, ok):
    with STEPS.open("a", encoding="utf-8") as handle:
        print(json.dumps({"t": round(time.time() - T0, 2), "step": name, "ok": ok}), file=handle)


def descendants(node):
    yield node
    for child in node:
        yield from descendants(child)


def find(pattern, role=None):
    desktop = pyatspi.Registry.getDesktop(0)
    for app in desktop:
        if app.get_process_id() == PID:
            for node in descendants(app):
                if re.search(pattern, node.name or "") and (role is None or node.getRoleName() == role):
                    return node


def await_node(pattern, role=None):
    until = time.monotonic() + 15
    while time.monotonic() < until:
        node = find(pattern, role)
        if node:
            return node
        time.sleep(0.25)
    raise RuntimeError("No accessible element matches " + pattern)


def press(node):
    action = node.queryAction()
    if not action.nActions or not action.doAction(0):
        raise RuntimeError("Cannot activate " + node.name)


name = "open the Telegram conversation using its accessible row"
try:
    press(await_node(r"^Telegram\b", "push button"))
    press(await_node(r"^Ines Marchetti(?:,|$)", "list item"))
    step(name, True)
    name = "read the Spanish message with sender and time"
    message = await_node("Ines Marchetti, .*\\d.*horario del ferry", "list item")
    await_node("^Message$")
    step(name, True)
    name = "activate Translate from the message menu"
    if not message.queryComponent().grabFocus():
        raise RuntimeError("The message row does not take focus")
    pyatspi.Registry.generateKeyboardEvent(0xff0d, None, pyatspi.KEY_SYM)
    press(await_node("^Translate$", "menu item"))
    await_node("The ferry schedule changed")
    await_node("Translated from Spanish")
    step(name, True)
    name = "Show original removes the translation and preserves the message"
    press(await_node("^Show original$", "push button"))
    await_node("Ines Marchetti, .*horario del ferry", "list item")
    deadline = time.monotonic() + 5
    while find("The ferry schedule changed|^Show original$") and time.monotonic() < deadline:
        time.sleep(0.25)
    if find("The ferry schedule changed|^Show original$"):
        raise RuntimeError("Translation is still visible")
    step(name, True)
except Exception as error:
    step(name, False)
    sys.exit(str(error))
