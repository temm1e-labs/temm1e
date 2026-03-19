from typing import List

from .models import Board, Task, TaskStatus, Priority


def search_tasks(board: Board, query: str) -> List[Task]:
    tasks = list(getattr(board, "tasks", {}).values())
    query_lower = query.lower()
    
    result = []
    for task in tasks:
        if query_lower in task.title.lower() or query_lower in task.description.lower():
            result.append(task)
            
    return result


def filter_by_status(board: Board, status: TaskStatus) -> List[Task]:
    tasks = list(getattr(board, "tasks", {}).values())
    return [task for task in tasks if task.status == status]


def filter_by_priority(board: Board, priority: Priority) -> List[Task]:
    tasks = list(getattr(board, "tasks", {}).values())
    return [task for task in tasks if task.priority == priority]