```python
from typing import Optional, Any

from .models import Board, Column, Task, Priority, TaskStatus

# Monkey-patch Board to support tasks dictionary persistence
_original_board_to_dict = Board.to_dict
_original_board_from_dict = Board.from_dict

def _board_to_dict(self: Board) -> dict[str, Any]:
    data = _original_board_to_dict(self)
    tasks_dict = getattr(self, "tasks", {})
    data["tasks"] = {tid: task.to_dict() for tid, task in tasks_dict.items()}
    return data

@classmethod
def _board_from_dict(cls, data: dict[str, Any]) -> Board:
    board = _original_board_from_dict(data)
    tasks_data = data.get("tasks", {})
    board.tasks = {tid: Task.from_dict(tdata) for tid, tdata in tasks_data.items()}
    return board

Board.to_dict = _board_to_dict
Board.from_dict = _board_from_dict


def create_board(name: str) -> Board:
    board = Board(name=name)
    board.tasks = {}
    return board


def add_column(board: Board, name: str) -> Column:
    column = Column(name=name