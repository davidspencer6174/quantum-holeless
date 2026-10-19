"""Read-only local browser for concrete per-root search records."""

import argparse
import csv
import io
import json
from pathlib import Path
import re
import threading
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


ASSETS = Path(__file__).resolve().parent / "level-player"
DIRECTIONS = {"U": (0, -1, 1), "D": (0, 1, 2), "L": (-1, 0, 4), "R": (1, 0, 8)}


def replay(level):
    """Validate saved witnesses using the solver's directional push rules."""
    width, height = level["width"], level["height"]
    boxes = {box["position"]: box["mask"] for box in level["boxes"]}
    walls, player = set(level["walls"]), level["player"]

    def neighbor(position, dx, dy):
        x, y = position % width + dx, position // width + dy
        if 0 <= x < width and 0 <= y < height:
            return y * width + x
        return None

    for direction in level["solution"]:
        if player == level["exit"]:
            raise ValueError("Solution continues after reaching exit")
        dx, dy, bit = DIRECTIONS[direction]
        dest = neighbor(player, dx, dy)
        if dest is None or dest in walls:
            raise ValueError("Solution walks into a wall")
        if dest in boxes:
            beyond = neighbor(dest, dx, dy)
            if not boxes[dest] & bit or beyond is None or beyond in walls or beyond in boxes:
                raise ValueError("Solution contains an illegal push")
            boxes[beyond] = boxes.pop(dest)
        player = dest
    if player != level["exit"]:
        raise ValueError("Solution does not reach exit")


def parse_record(text):
    header, body = text.replace("\r\n", "\n").split("\n\n", 1)
    row = next(csv.DictReader(io.StringIO(header), delimiter="\t"))
    if "witness=concrete" not in body.splitlines():
        return None
    route = re.search(r"^steps=(\d+) solution=([UDLR]*)$", body, re.MULTILINE)
    if not route:
        raise ValueError("Missing saved solution")
    lines = [line.strip() for line in body.split("Input:\n", 1)[1].splitlines() if line.strip()]
    exit_match = re.fullmatch(r"exit (\d+),(\d+)", lines[0])
    if not exit_match or not re.fullmatch(r"steps \d+", lines[1]):
        raise ValueError("Invalid input header")
    grid, mask_rows = [], []
    for line in lines[2:]:
        if re.fullmatch(r"[0-4]+", line) and not mask_rows:
            grid.append(line)
        else:
            mask_rows.append(line.split())
    width, height = len(grid[0]), len(grid)
    if any(len(line) != width for line in grid) or len(mask_rows) != height:
        raise ValueError("Inconsistent grid dimensions")
    if any(len(line) != width for line in mask_rows):
        raise ValueError("Inconsistent push mask dimensions")
    ex, ey = map(int, exit_match.groups())
    if not (1 <= ex <= width and 1 <= ey <= height):
        raise ValueError("Exit outside board")
    tiles = "".join(grid)
    if tiles.count("4") != 1:
        raise ValueError("Expected one player")
    boxes = []
    for position, tile in enumerate(tiles):
        if tile == "2":
            token = mask_rows[position // width][position % width]
            if len(token) != 4 or any(c not in (".", direction) for c, direction in zip(token, "UDLR")):
                raise ValueError("Invalid box mask")
            boxes.append({"position": position, "mask": sum(bit for c, (_, _, bit) in zip(token, DIRECTIONS.values()) if c != ".")})
    level = dict(id=int(row["id"]), prefix=row["prefix"], status=row["status"],
                 steps=int(route[1]), solution=route[2], width=width, height=height,
                 exit=(ey - 1) * width + ex - 1, player=tiles.index("4"),
                 walls=[i for i, tile in enumerate(tiles) if tile == "1"], boxes=boxes)
    if level["steps"] != len(level["solution"]) or level["steps"] != int(row["best_steps"]):
        raise ValueError("Inconsistent solution length")
    if tiles[level["exit"]] in "12":
        raise ValueError("Exit starts covered")
    replay(level)
    return level


class Records:
    def __init__(self, directory):
        self.directory = directory
        self.cache = {}
        self.lock = threading.Lock()

    def snapshot(self):
        # Reuse last valid snapshots while a worker is rewriting an export.
        with self.lock:
            warnings = []
            for path in sorted((self.directory / "subtrees").glob("root-*.txt")):
                try:
                    stat = path.stat()
                    stamp = (stat.st_mtime_ns, stat.st_size)
                    if path in self.cache and self.cache[path][0] == stamp:
                        continue
                    level = parse_record(path.read_text(encoding="utf-8-sig"))
                    self.cache[path] = (stamp, level)
                except (OSError, ValueError, KeyError, IndexError, StopIteration) as error:
                    warnings.append(f"{path.name}: {error}")
            levels = [level for _, level in self.cache.values() if level is not None]
            return {"source": str(self.directory), "levels": sorted(levels, key=lambda level: level["id"]),
                    "warnings": warnings, "skipped": sum(level is None for _, level in self.cache.values())}


def find_records(value):
    if value:
        path = Path(value).resolve()
        if (path / "search-records").is_dir():
            path /= "search-records"
        if not (path / "subtrees").is_dir():
            raise ValueError(f"No search-records/subtrees directory at {path}")
        return path
    root = Path(__file__).resolve().parent.parent
    candidates = [path.parent for base in (root / "runs", root / "logs")
                  for path in base.glob("*/search-records/subtrees")]
    if not candidates:
        raise ValueError("No root records found. Supply --records CHECKPOINT_DIRECTORY")
    return max(candidates, key=lambda path: max((p.stat().st_mtime for p in (path / "subtrees").glob("root-*.txt")), default=path.stat().st_mtime))


def serve(records, port):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            route = self.path.split("?", 1)[0]
            if route == "/api/levels":
                content, mime = json.dumps(records.snapshot()).encode(), "application/json"
            elif route in ("/", "/app.js", "/style.css", "/lucide.min.js"):
                name, mime = {"/": ("index.html", "text/html"), "/app.js": ("app.js", "text/javascript"),
                              "/style.css": ("style.css", "text/css"),
                              "/lucide.min.js": ("lucide.min.js", "text/javascript")}[route]
                content = (ASSETS / name).read_bytes()
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", mime + "; charset=utf-8")
            self.send_header("Content-Length", str(len(content)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.end_headers()
            self.wfile.write(content)

        def log_message(self, *_):
            pass

    if port:
        return ThreadingHTTPServer(("127.0.0.1", port), Handler)
    for candidate in range(8765, 8865):
        try:
            return ThreadingHTTPServer(("127.0.0.1", candidate), Handler)
        except OSError:
            continue
    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--records", help="Checkpoint directory or its search-records directory")
    parser.add_argument("--port", type=int, default=0, help="Default: first free port starting at 8765")
    parser.add_argument("--no-browser", action="store_true")
    args = parser.parse_args()
    try:
        records = Records(find_records(args.records))
    except ValueError as error:
        parser.error(str(error))
    server = serve(records, args.port)
    url = f"http://127.0.0.1:{server.server_port}"
    print(f"Root level player: {url}\nRecords: {records.directory}\nClose this terminal or press Ctrl+C to stop the player.", flush=True)
    if not args.no_browser:
        webbrowser.open_new_tab(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
