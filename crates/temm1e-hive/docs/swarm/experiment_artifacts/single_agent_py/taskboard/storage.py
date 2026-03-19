import json
from pathlib import Path

try:
    from .models import Board
except ImportError:
    from models import Board


class Storage:
    def __init__(self, path: str) -> None:
        self.path: Path = Path(path)

    def save(self, board: Board) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        board.save(self.path)

    def load(self) -> Board:
        if not self.exists():
            return Board(name="Default Board")
        return Board.load(self.path)

    def exists(self) -> bool:
        return self.path.exists() and self.path.is_file()