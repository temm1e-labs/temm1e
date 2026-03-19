import json
import os
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any


class TaskStatus(str, Enum):
    TODO = "TODO"
    IN_PROGRESS = "IN_PROGRESS"
    DONE = "DONE"


class Priority(str, Enum):
    LOW = "LOW"
    MEDIUM = "MEDIUM"
    HIGH = "HIGH"
    CRITICAL = "CRITICAL"


@dataclass
class Task:
    title: str
    description: str
    column_id: str
    status: TaskStatus = TaskStatus.TODO
    priority: Priority = Priority.MEDIUM
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    created_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "description": self.description,
            "status": self.status.value,
            "priority": self.priority.value,
            "created_at": self.created_at,
            "column_id": self.column_id,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Task":
        return cls(
            id=data.get("id", str(uuid.uuid4())),
            title=data["title"],
            description=data["description"],
            status=TaskStatus(data.get("status", TaskStatus.TODO.value)),
            priority=Priority(data.get("priority", Priority.MEDIUM.value)),
            created_at=data.get("created_at", datetime.now(timezone.utc).isoformat()),
            column_id=data["column_id"],
        )


@dataclass
class Column:
    name: str
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    task_ids: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "task_ids": self.task_ids,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Column":
        return cls(
            id=data.get("id", str(uuid.uuid4())),
            name=data["name"],
            task_ids=data.get("task_ids", []),
        )


@dataclass
class Board:
    name: str
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    columns: list[Column] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "columns": [col.to_dict() for col in self.columns],
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Board":
        return cls(
            id=data.get("id", str(uuid.uuid4())),
            name=data["name"],
            columns=[Column.from_dict(col_data) for col_data in data.get("columns", [])],
        )

    def save(self, file_path: Path) -> None:
        file_path.parent.mkdir(parents=True, exist_ok=True)
        with file_path.open("w", encoding="utf-8") as f:
            json.dump(self.to_dict(), f, indent=2)

    @classmethod
    def load(cls, file_path: Path) -> "Board":
        if not file_path.exists():
            raise FileNotFoundError(f"Board file not found: {file_path}")
        with file_path.open("r", encoding="utf-8") as f:
            data = json.load(f)
        return cls.from_dict(data)