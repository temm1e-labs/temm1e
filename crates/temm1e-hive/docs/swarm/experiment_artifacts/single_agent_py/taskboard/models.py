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


def _generate_uuid() -> str:
    return str(uuid.uuid4())


def _generate_timestamp() -> str:
    return datetime.now(timezone.utc).isoformat()


@dataclass
class Task:
    title: str
    description: str
    status: TaskStatus
    priority: Priority
    column_id: str
    id: str = field(default_factory=_generate_uuid)
    created_at: str = field(default_factory=_generate_timestamp)

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
            id=data.get("id", _generate_uuid()),
            title=data["title"],
            description=data["description"],
            status=TaskStatus(data["status"]),
            priority=Priority(data["priority"]),
            created_at=data.get("created_at", _generate_timestamp()),
            column_id=data["column_id"],
        )


@dataclass
class Column:
    name: str
    id: str = field(default_factory=_generate_uuid)
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
            id=data.get("id", _generate_uuid()),
            name=data["name"],
            task_ids=data.get("task_ids", []),
        )


@dataclass
class Board:
    name: str
    id: str = field(default_factory=_generate_uuid)
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
            id=data.get("id", _generate_uuid()),
            name=data["name"],
            columns=[Column.from_dict(col) for col in data.get("columns", [])],
        )

    def save(self, file_path: Path) -> None:
        with file_path.open("w", encoding="utf-8") as f:
            json.dump(self.to_dict(), f, indent=4)

    @classmethod
    def load(cls, file_path: Path) -> "Board":
        with file_path.open("r", encoding="utf-8") as f:
            data = json.load(f)
        return cls.from_dict(data)