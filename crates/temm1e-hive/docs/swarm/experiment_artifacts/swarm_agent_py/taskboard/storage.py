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
        with self.path.open("w", encoding="utf-8") as f:
            json.dump(board.to_dict(), f, indent=2)

    def load(self) -> Board:
        if not self.exists():
            return Board(name="New Board")
        
        with self.path.open("r", encoding="utf-8") as f:
            data = json.load(f)
            
        return Board.from_dict(data)

    def exists(self) -> bool:
        return self.path.exists()