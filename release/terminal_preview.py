"""Small POSIX PTY driver for native flow checks; text checks are not screenshots."""

import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import termios
import time

TOKENS = re.compile(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\)|[()][A-Z0-9]|.)|.', re.S)


def screen(data, columns=100, rows=32):
    grid = [[' '] * columns for _ in range(rows)]
    x = y = 0
    for match in TOKENS.finditer(data.decode('utf-8', 'replace')):
        token = match.group()
        if token.startswith('\x1b['):
            op, raw = token[-1], token[2:-1]
            parts = [int(part) if part.isdigit() else 0 for part in raw.split(';')]
            count = parts[0] or 1
            if op in ('H', 'f'):
                y, x = min(rows - 1, count - 1), min(columns - 1, (parts[1] if len(parts) > 1 else 1) - 1)
            elif op == 'G': x = min(columns - 1, count - 1)
            elif op == 'd': y = min(rows - 1, count - 1)
            elif op == 'A': y = max(0, y - count)
            elif op == 'B': y = min(rows - 1, y + count)
            elif op == 'C': x = min(columns - 1, x + count)
            elif op == 'D': x = max(0, x - count)
            elif (op == 'J' and raw in ('2', '3')) or (op in ('h', 'l') and raw == '?1049'):
                grid = [[' '] * columns for _ in range(rows)]
                x = y = 0
            elif op == 'K':
                for column in range(0 if raw == '2' else x, columns): grid[y][column] = ' '
        elif token.startswith('\x1b'): pass
        elif token == '\r': x = 0
        elif token == '\n': y = min(rows - 1, y + 1)
        elif token == '\b': x = max(0, x - 1)
        elif ord(token) >= 32:
            grid[y][x] = token
            x = min(columns - 1, x + 1)
    return '\n'.join(''.join(line).rstrip() for line in grid)


class Terminal:
    def __init__(self, command, cwd, env):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 100, 0, 0))
        self.raw = b''
        self.proc = subprocess.Popen([str(part) for part in command], cwd=cwd, env=env,
                                     stdin=slave, stdout=slave, stderr=slave,
                                     start_new_session=True,
                                     preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)

    def capture(self, seconds=0.3):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], min(0.1, max(0, deadline - time.monotonic())))[0]:
                try: chunk = os.read(self.master, 65536)
                except OSError: break
                if not chunk: break
                self.raw += chunk
                if len(self.raw) > 4_000_000: raise RuntimeError('unexpected terminal output volume')
        return screen(self.raw)

    def wait(self, text, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = self.capture()
            if text in result: return result
            if self.proc.poll() is not None: break
        raise AssertionError(f'Missing {text!r}:\n{screen(self.raw)}')

    def send(self, data):
        os.write(self.master, data)

    def click(self, label):
        text = self.capture()
        y, line = next((y, line) for y, line in enumerate(text.splitlines()) if label in line)
        x = line.index(label)
        self.send(f'\x1b[<0;{x + 1};{y + 1}M\x1b[<0;{x + 1};{y + 1}m'.encode())
        return self.capture()

    def finish(self):
        self.capture(0.5)  # Drain output before waiting: never block a terminal writer.
        self.proc.wait(timeout=5)
        assert self.proc.returncode == 0, screen(self.raw)

    def close(self):
        if self.proc.poll() is None:
            self.send(b'\x03')
            self.capture(0.5)
            try: self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.terminate()
                try: self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait(timeout=3)
        os.close(self.master)

    def __enter__(self): return self

    def __exit__(self, *_args): self.close()
