```python
import sys
from pathlib import Path
from typing import List

# Ensure the current directory is in sys.path to allow fallback imports to work
_current_dir = Path(__file__).resolve().parent
if str(_current_dir) not in sys.path:
    sys.path.insert(0, str(_current_dir))

try:
    # Try relative import first (works when imported as a package)
    from .models import Board, Priority, Task, TaskStatus
except ImportError:
    # Fallback to absolute import (works when run as a script or if sys.path is modified)
    from models import Board, Priority, Task, TaskStatus


def get_all_tasks(board: Board) -> List[Task]:
    """Retrieve all tasks from the board."""
    if hasattr(board, "tasks"):
        return list(board.tasks.values())  # type: ignore
    return []


def search_tasks(board: Board, query: str) -> List[Task]:
    query_lower = query.lower()
    return [
        task for task in get_all_tasks(board)
        if query_lower in task.title.lower() or query_lower in task.description.lower()
    ]


def filter_by_status(board: Board, status: TaskStatus) -> List[Task]:
    return [
        task for task in get_all_tasks(board)