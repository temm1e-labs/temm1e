```python
import sys
from pathlib import Path
from typing import Optional

# Ensure the current directory is in sys.path to allow fallback imports to work
_current_dir = Path(__file__).resolve().parent
if str(_current_dir) not in sys.path:
    sys.path.insert(0, str(_current_dir))

try:
    # Try relative import first (works when imported as a package)
    from .models import Board, Column, Priority, Task
except ImportError:
    # Fallback to absolute import (works when run as a script or if sys.path is modified)
    from models import Board, Column, Priority, Task


def _ensure_tasks_dict(board: Board) -> None:
    """Ensure the board has a dictionary to store Task objects."""
    if not hasattr(board, "tasks"):
        board.tasks = {}  # type: ignore


def create_board(name: str) -> Board:
    board = Board(name=name)
    _ensure_tasks_dict(board)
    return board


def add_column(board: Board, name: str) -> Column:
    column = Column(name=name)
    board.columns.append(column)
    return column


def add_task(
    board: Board, column_id: str, title: str, description: str, priority: Priority
) -> Task:
    _ensure_tasks_dict(