```python
import json
import sys
from pathlib import Path

_current_dir = Path(__file__).resolve().parent
_parent_dir = _current_dir.parent

# Add parent dir to sys.path to allow 'import taskboard.X' to work correctly
# without shadowing the package itself by adding _current_dir.
if str(_parent_dir) not in sys.path:
    sys.path.insert(0, str(_parent_dir))

try:
    from taskboard.models import Board
    from taskboard.board import _ensure_tasks_dict
except ImportError:
    try:
        from .models import Board
        from .board import _ensure_tasks_dict
    except ImportError:
        # Fallback if run as a standalone script
        if str(_current_dir) not in sys.path:
            sys.path.insert(0, str(_current_dir))
        from models import Board
        from board import _ensure_tasks_dict


def export_to_markdown(board: Board) -> str:
    _ensure_tasks_dict(board)
    lines = [f"# {board.name}"]
    
    for column in board.columns:
        lines.append(f"## {column.name}")
        for task_id in column.task_ids:
            task = board.tasks.get(task_id)  # type: ignore
            if task: