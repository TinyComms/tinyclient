"""A full-screen, animated source on A's first display."""
import time
import tkinter as tk

from pixels import BIT_CENTERS, COLORS

root = tk.Tk()
root.title("Tiny lab test card")
root.attributes("-fullscreen", True)
root.attributes("-topmost", True)
canvas = tk.Canvas(root, highlightthickness=0)
canvas.pack(fill="both", expand=True)
started = time.monotonic()


def draw():
    w, h = canvas.winfo_width(), canvas.winfo_height()
    if w < 10 or h < 10:
        root.after(100, draw)
        return
    canvas.delete("all")
    for i, color in enumerate(COLORS):
        x, y = (i % 2) * w / 2, (i // 2) * h / 2
        canvas.create_rectangle(x, y, x + w / 2, y + h / 2, fill=color, width=0)
    number = int(time.monotonic() - started) % 64
    canvas.create_rectangle(w * .30, h * .33, w * .70, h * .66, fill="black", width=0)
    canvas.create_text(w / 2, h * .46, text=str(number), fill="white",
                       font=("TkDefaultFont", max(24, int(h * .15)), "bold"))
    for bit, x in enumerate(BIT_CENTERS):
        if number & (1 << bit):
            canvas.create_rectangle(w * (x - 2) / 100, h * .58,
                                    w * (x + 2) / 100, h * .64, fill="white", width=0)
    root.after(100, draw)


draw()
root.mainloop()
