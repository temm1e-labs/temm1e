import pytest
from pathlib import Path

from taskboard.models import Board, Column, Task, TaskStatus, Priority
from taskboard.board import create_board, add_column, add_task, move_task
from taskboard.search import search_tasks, filter_by_status, filter_by_priority
from taskboard.export import export_markdown
from taskboard.storage import Storage


def test_create_board() -> None:
    board = create_board("My Test Board")
    assert board.name == "My Test Board"
    assert isinstance(board, Board)
    assert getattr(board, "tasks", {}) == {}
    assert board.columns == []


def test_add_column() -> None:
    board = create_board("My Test Board")
    col1 = add_column(board, "To Do")
    col2 = add_column(board, "In Progress")
    
    assert len(board.columns) == 2
    assert board.columns[0].name == "To Do"
    assert board.columns[1].name == "In Progress"
    assert board.columns[0].id == col1.id
    assert board.columns[1].id == col2.id


def test_add_task() -> None:
    board = create_board("My Test Board")
    col = add_column(board, "To Do")
    
    task = add_task(
        board=board,
        column_id=col.id,
        title="Implement Login",
        description="Add JWT authentication",
        status=TaskStatus.TODO,
        priority=Priority.HIGH
    )
    
    assert task.title == "Implement Login"
    assert task.id in board.tasks
    assert task.id in col.task_ids
    assert task.column_id == col.id
    assert board.tasks[task.id] == task


def test_move_task() -> None:
    board = create_board("My Test Board")
    col1 = add_column(board, "To Do")
    col2 = add_column(board, "Done")
    
    task = add_task(
        board=board,
        column_id=col1.id,
        title="Fix Bug",
        description="Null pointer exception",
        status=TaskStatus.TODO,
        priority=Priority.CRITICAL
    )
    
    assert task.id in col1.task_ids
    
    move_task(board, task.id, col2.id)
    
    assert task.id not in col1.task_ids
    assert task.id in col2.task_ids
    assert task.column_id == col2.id
    assert board.tasks[task.id].column_id == col2.id


def test_search() -> None:
    board = create_board("My Test Board")
    col = add_column(board, "To Do")
    
    add_task(board, col.id, "Find the magic ring", "In a volcano", TaskStatus.TODO, Priority.CRITICAL)
    add_task(board, col.id, "Buy groceries", "Milk and eggs", TaskStatus.TODO, Priority.LOW)
    add_task(board, col.id, "Clean room", "Vacuum the floor", TaskStatus.TODO, Priority.MEDIUM)
    
    results_title = search_tasks(board, "magic")
    assert len(results_title) == 1
    assert results_title[0].title == "Find the magic ring"
    
    results_desc = search_tasks(board, "eggs")
    assert len(results_desc) == 1
    assert results_desc[0].title == "Buy groceries"
    
    results_none = search_tasks(board, "dragon")
    assert len(results_none) == 0


def test_filter_status() -> None:
    board = create_board("My Test Board")
    col = add_column(board, "Tasks")
    
    add_task(board, col.id, "Task 1", "Desc", TaskStatus.TODO, Priority.LOW)
    add_task(board, col.id, "Task 2", "Desc", TaskStatus.DONE, Priority.HIGH)
    add_task(board, col.id, "Task 3", "Desc", TaskStatus.DONE, Priority.MEDIUM)
    
    todo_tasks = filter_by_status(board, TaskStatus.TODO)
    assert len(todo_tasks) == 1
    assert todo_tasks[0].title == "Task 1"
    
    done_tasks = filter_by_status(board, TaskStatus.DONE)
    assert len(done_tasks) == 2
    
    in_progress_tasks = filter_by_status(board, TaskStatus.IN_PROGRESS)
    assert len(in_progress_tasks) == 0


def test_export_markdown() -> None:
    board = create_board("Export Board")
    col = add_column(board, "Review")
    add_task(board, col.id, "Review PR #42", "Check for memory leaks", TaskStatus.IN_PROGRESS, Priority.HIGH)
    
    md_output = export_markdown(board)
    
    assert isinstance(md_output, str)
    assert "Export Board" in md_output or "Review" in md_output
    assert "Review" in md_output
    assert "Review PR #42" in md_output


def test_storage_roundtrip(tmp_path: Path) -> None:
    board = create_board("Storage Board")
    col = add_column(board, "Backlog")
    add_task(board, col.id, "Persist me", "Please save this task", TaskStatus.TODO, Priority.CRITICAL)
    
    db_path = tmp_path / "test_board.json"
    storage = Storage(str(db_path))
    
    assert not storage.exists()
    storage.save(board)
    assert storage.exists()
    
    new_storage = Storage(str(db_path))
    loaded_board = new_storage.load()
    
    assert loaded_board.name == "Storage Board"
    assert len(loaded_board.columns) == 1
    assert loaded_board.columns[0].name == "Backlog"
    
    loaded_tasks = getattr(loaded_board, "tasks", {})
    assert len(loaded_tasks) == 1
    
    task_id = list(loaded_tasks.keys())[0]
    assert loaded_tasks[task_id].title == "Persist me"
    assert loaded_tasks[task_id].description == "Please save this task"
    assert loaded_tasks[task_id].status == TaskStatus.TODO
    assert loaded_tasks[task_id].priority == Priority.CRITICAL
    assert loaded_tasks[task_id].column_id == loaded_board.columns[0].id