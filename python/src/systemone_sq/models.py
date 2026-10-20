"""Native System One questions and validated, typed answers."""

from __future__ import annotations

import math
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal, TypeAlias

JSON: TypeAlias = str | int | float | bool | None | list["JSON"] | dict[str, "JSON"]


def _text(value: str, name: str) -> None:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be nonblank text")


@dataclass(frozen=True)
class Choice:
    instructions: str
    criteria: Mapping[str, str | None]
    type: Literal["choice"] = field(default="choice", init=False)

    def __post_init__(self) -> None:
        _text(self.instructions, "instructions")
        if not 2 <= len(self.criteria) <= 255:
            raise ValueError("Choice needs 2..255 distinct options")
        for key, description in self.criteria.items():
            _text(key, "choice label")
            if description is not None and not isinstance(description, str):
                raise ValueError("choice descriptions must be strings or None")
        object.__setattr__(self, "criteria", dict(self.criteria))

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": self.type,
            "instructions": self.instructions,
            "criteria": dict(self.criteria),
        }


@dataclass(frozen=True)
class Score:
    instructions: str
    criteria: Sequence[str]
    type: Literal["score"] = field(default="score", init=False)

    def __post_init__(self) -> None:
        _text(self.instructions, "instructions")
        if isinstance(self.criteria, str) or not 2 <= len(self.criteria) <= 10:
            raise ValueError("Score needs 2..10 ordered level descriptions")
        for level in self.criteria:
            _text(level, "score level")
        object.__setattr__(self, "criteria", tuple(self.criteria))

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": self.type,
            "instructions": self.instructions,
            "criteria": list(self.criteria),
        }


@dataclass(frozen=True)
class Noul:
    instructions: str
    criteria: Mapping[str, str] | None = None
    type: Literal["noul"] = field(default="noul", init=False)

    def __post_init__(self) -> None:
        _text(self.instructions, "instructions")
        if self.criteria is not None:
            if set(self.criteria) != {"true", "false"}:
                raise ValueError("Noul criteria must contain exactly true and false")
            for value in self.criteria.values():
                _text(value, "noul description")
            object.__setattr__(self, "criteria", dict(self.criteria))

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {"type": self.type, "instructions": self.instructions}
        if self.criteria is not None:
            result["criteria"] = dict(self.criteria)
        return result


Question: TypeAlias = Choice | Score | Noul


@dataclass(frozen=True)
class ChoiceAnswer:
    choice: str
    probabilities: Mapping[str, float]
    confidence: float
    type: Literal["choice"] = field(default="choice", init=False)


@dataclass(frozen=True)
class ScoreAnswer:
    score: float
    probabilities: Mapping[str, float]
    confidence: float
    legend: tuple[str, ...]
    type: Literal["score"] = field(default="score", init=False)

    def scaled(self, maximum: float = 100) -> float:
        if not math.isfinite(maximum) or maximum <= 0:
            raise ValueError("maximum must be positive and finite")
        return self.score / (len(self.legend) - 1) * maximum


@dataclass(frozen=True)
class NoulAnswer:
    """noul is P(true). It is not a confidence statistic."""

    noul: float
    type: Literal["noul"] = field(default="noul", init=False)


Answer: TypeAlias = ChoiceAnswer | ScoreAnswer | NoulAnswer


@dataclass(frozen=True)
class Result:
    answers: Mapping[str, Answer]
    model: str | None
    usage: Mapping[str, Any]
    provider: str
    raw: Mapping[str, Any] = field(repr=False)

    def passes(
        self, question_id: str, *, confidence: float | None = None, threshold: float | None = None
    ) -> bool:
        answer = self.answers[question_id]
        if confidence is not None:
            _number(confidence, 0, 1)
            if isinstance(answer, NoulAnswer):
                raise ValueError("Noul has no confidence; use threshold for P(true)")
        if threshold is not None:
            if isinstance(answer, ChoiceAnswer):
                raise ValueError("threshold requires Score or Noul")
            _number(threshold, 0, len(answer.legend) - 1 if isinstance(answer, ScoreAnswer) else 1)
        if confidence is not None and not isinstance(answer, NoulAnswer):
            if answer.confidence < confidence:
                return False
        if threshold is not None:
            if isinstance(answer, ChoiceAnswer):
                raise ValueError("threshold requires Score or Noul")
            value = answer.score if isinstance(answer, ScoreAnswer) else answer.noul
            if value < threshold:
                return False
        return True


def _number(value: Any, low: float, high: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (float, int)):
        raise ValueError("missing or nonnumeric answer field")
    if not math.isfinite(value) or not low <= value <= high:
        raise ValueError("answer field outside permitted range")
    return float(value)


def parse_result(data: Any, questions: Mapping[str, Question], provider: str) -> Result:
    if not isinstance(data, dict) or not isinstance(data.get("answers"), dict):
        raise ValueError("response must contain an answers object")
    if set(data["answers"]) != set(questions):
        raise ValueError("response answer ids differ from request")
    answers: dict[str, Answer] = {}
    for key, question in questions.items():
        a = data["answers"][key]
        if not isinstance(a, dict) or a.get("type") != question.type:
            raise ValueError("response answer type differs from request")
        if isinstance(question, Noul):
            answers[key] = NoulAnswer(_number(a.get("noul"), 0, 1))
            continue
        p = a.get("probabilities")
        expected = (
            set(question.criteria)
            if isinstance(question, Choice)
            else {str(i) for i in range(len(question.criteria))}
        )
        if not isinstance(p, dict) or set(p) != expected:
            raise ValueError("response probability labels differ from criteria")
        probabilities = {k: _number(v, 0, 1) for k, v in p.items()}
        if abs(sum(probabilities.values()) - 1) > 0.01:
            raise ValueError("probabilities do not sum to one")
        confidence = _number(a.get("confidence"), 0, 1)
        if isinstance(question, Choice):
            if a.get("choice") not in question.criteria:
                raise ValueError("response selected an undeclared choice")
            answers[key] = ChoiceAnswer(a["choice"], probabilities, confidence)
        else:
            legend = a.get("legend")
            if isinstance(legend, dict):
                if set(legend) != expected:
                    raise ValueError("invalid score legend indices")
                legend = [legend[str(i)] for i in range(len(question.criteria))]
            if legend != list(question.criteria):
                raise ValueError("response legend differs from requested levels")
            answers[key] = ScoreAnswer(
                _number(a.get("score"), 0, len(question.criteria) - 1),
                probabilities,
                confidence,
                tuple(legend),
            )
    model = data.get("model")
    if model is not None and not isinstance(model, str):
        raise ValueError("model must be a string")
    usage = data.get("usage", {})
    if not isinstance(usage, dict):
        raise ValueError("usage must be an object")
    metadata = data.get("_sq")
    if isinstance(metadata, dict) and isinstance(metadata.get("provider"), str):
        provider = metadata["provider"]
    return Result(answers, model, usage, provider, data)
