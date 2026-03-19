from pathlib import Path

from taskboard.board import add_column, add_task, create_board, move_task
from taskboard.export import export_to_markdown
from taskboard.models import Priority, TaskStatus
from taskboard.search import filter_by_status, search_tasks
from taskboard.storage import Storage


def test_create_board() -> None:
    board = create_board("My Test Board")
    assert board.name == "My Test Board"
    assert hasattr(board, "tasks")
    assert isinstance(board.tasks, dict)


def test_add_column() -> None:
    board = create_board("Board with Columns")
    col1 = add_column(board, "To Do")
    col2 = add_column(board, "In Progress")
    
    assert len(board.columns) == 2
    assert board.columns[0].name == "To Do"
    assert board.columns[1].name == "In Progress"
    assert board.columns[0].id == col1.id
    assert board.columns[1].id == col2.id


def test_add_task() -> None:
    board = create_board("Board with Tasks")
    col = add_column(board, "To Do")
    task = add_task(
        board=board,
        column_id=col.id,
        title="Write Tests",
        description="Write pytest tests for the taskboard",
        priority=Priority.HIGH,
    )
    
    assert task.title == "Write Tests"
    assert task.description == "Write pytest tests for the taskboard"
    assert task.priority == Priority.HIGH
    assert task.status == TaskStatus.TODO
    assert task.id in col.task_ids
    assert board.tasks[task.id] == task


def test_move_task() -> None:
    board = create_board("Board for Moving")
    col1 = add_column(board, "To Do")
    col2 = add_column(board, "Done")
    
    task = add_task(
        board=board,
        column_id=col1.id,
        title="Task to Move",
        description="This task will be moved",
        priority=Priority.MEDIUM,
    )
    
    assert task.id in col1.task_ids
    assert task.id not in col2.task_ids
    
    move_task(board, task.id, col2.id)
    
    assert task.id not in col1.task_ids
    assert task.id in col2.task_ids
    assert task.column_id == col2.id


def test_search() -> None:
    board = create_board("Board for Searching")
    col = add_column(board, "To Do")
    
    add_task(board, col.id, "Buy Groceries", "Milk, Eggs, Bread", Priority.MEDIUM)
    add_task(board, col.id, "Write Code", "Implement search feature", Priority.HIGH)
    add_task(board, col.id, "Fix Bug", "Fix the grocery list bug", Priority.CRITICAL)
    
    results_groceries = search_tasks(board, "grocer")
    assert len(results_groceries) == 2
    titles = {t.title for t in results_groceries}
    assert "Buy Groceries" in titles
    assert "Fix Bug" in titles
    
    results_code = search_tasks(board, "implement")
    assert len(results_code) == 1
    assert results_code[0].title == "Write Code"


def test_filter_status() -> None:
    board = create_board("Board for Filtering")
    col = add_column(board, "General")
    
    task1 = add_task(board, col.id, "Task 1", "Desc 1", Priority.LOW)
    task2 = add_task(board, col.id, "Task 2", "Desc 2", Priority.LOW)
    task3 = add_task(board, col.id, "Task 3", "Desc 3", Priority.LOW)
    
    task2.status = TaskStatus.IN_PROGRESS
    task3.status = TaskStatus.DONE
    
    todo_tasks = filter_by_status(board, TaskStatus.TODO)
    assert len(todo_tasks) == 1
    assert todo_tasks[0].id == task1.id
    
    in_progress_tasks = filter_by_status(board, TaskStatus.IN_PROGRESS)
    assert len(in_progress_tasks) == 1
    assert in_progress_tasks[0].id == task2.id


def test_export_markdown() -> None:
    board = create_board("Project Alpha")
    col = add_column(board, "Backlog")
    add_task(board, col.id, "Setup Database", "PostgreSQL setup", Priority.CRITICAL)
    
    markdown_output = export_to_markdown(board)
    
    assert "# Project Alpha" in markdown_output
    assert "## Backlog" in markdown_output
    assert "Setup Database" in markdown_output


def test_storage_roundtrip(tmp_path: Path) -> None:
    board = create_board("Persistent Board")
    col1 = add_column(board, "To Do")
    col2 = add_column(board, "Done")
    
    file_path = tmp_path / "test_board.json"
    storage = Storage(str(file_path))
    
    storage.save(board)
    assert storage.exists()
    
    loaded_board = storage.load()
    
    assert loaded_board.name == "Persistent Board"
    assert len(loaded_board.columns) == 2
    assert loaded_board.columns[0].name == "To Do"
    assert loaded_board.columns[1].name == "Done"
    assert loaded_board.columns[0].id == col1.id
    assert loaded_board.columns[1].id == col2.id