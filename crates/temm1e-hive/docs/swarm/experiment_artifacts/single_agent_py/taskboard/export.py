```python
import json
import sys
from pathlib import Path
from typing import Any

# Ensure the parent directory of 'taskboard' is in sys.path to resolve ModuleNotFoundError
_parent_dir = Path(__file__).resolve().parent.parent
if str(_parent_dir) not in sys.path:
    sys.path.insert(0, str(_parent_dir))

try:
    from taskboard.models import Board, Column, Task, Priority, TaskStatus
except ModuleNotFoundError:
    try:
        from .models import Board, Column, Task, Priority, TaskStatus
    except ImportError:
        from models import Board, Column, Task, Priority, TaskStatus

# Monkey-patch Board to support tasks dictionary persistence
# We do this here safely to avoid importing board.py which may contain syntax errors
if Board.to_dict.__name__ != "_board_to_dict":
    _original_board_to_dict = Board.to_dict
    _original_board_from_dict = Board.from_dict

    def _board_to_dict(self: Board) -> dict[str, Any]:
        data = _original_board_to_dict(self)
        tasks_dict = getattr(self, "tasks", {})
        data["tasks"] = {tid: task.to_dict() for tid, task in tasks_dict.items()}
        return data

    @classmethod
    def _board