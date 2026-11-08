"""The cost ledger: one JSONL line per paid request, with a hard budget.

Prices are USD per 1M tokens. The budget stops a request *before* it is sent:
an estimate that would cross the provider's budget raises :class:`BudgetExceeded`.
The ledger is append-only and machine-local (``local/brain/ledger.jsonl``).
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict

from . import config

#: (input USD per 1M tokens, output USD per 1M tokens)
PRICES: Dict[str, tuple[float, float]] = {
    "gpt-6-luna": (0.10, 0.50),
    "gpt-6.1-terra": (1.00, 5.00),
    "gpt-6.1-sol": (2.00, 10.00),
    "jev-latest": (0.042, 0.0),
}

#: Default budgets in USD. The user gave one $5 key each; keep a margin.
DEFAULT_BUDGETS: Dict[str, float] = {"openai": 4.50, "typesafe": 4.50}
BUDGET_ENV: Dict[str, str] = {
    "openai": "PRAXION_BUDGET_OPENAI",
    "typesafe": "PRAXION_BUDGET_TYPESAFE",
}

class BudgetExceeded(RuntimeError):
    """The next request would cross the provider's budget."""


def price(model: str) -> tuple[float, float]:
    try:
        return PRICES[model]
    except KeyError as error:
        raise ValueError(f"no price for model {model!r}; add it to ledger.PRICES") from error


def cost(model: str, input_tokens: int, output_tokens: int) -> float:
    """The USD cost of one request, by the model's price list."""
    input_price, output_price = price(model)
    return (
        input_tokens * input_price / 1_000_000
        + output_tokens * output_price / 1_000_000
    )


def estimate_cost(model: str, input_tokens: int, output_tokens: int = 0) -> float:
    return cost(model, input_tokens, output_tokens)


class Ledger:
    """Append-only cost log with a per-provider budget."""

    def __init__(self, path: Path | None = None) -> None:
        self.path = Path(path) if path is not None else config.ledger_path()

    # ------------------------------------------------------------------ #
    def entries(self) -> list[Dict[str, Any]]:
        try:
            text = self.path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return []
        entries = []
        for number, line in enumerate(text.splitlines(), start=1):
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"corrupt ledger line {number} in {self.path}: {error}") from error
            if not isinstance(entry, dict):
                raise ValueError(f"corrupt ledger line {number} in {self.path}: not an object")
            entries.append(entry)
        return entries

    def record(
        self,
        provider: str,
        model: str,
        input_tokens: int,
        output_tokens: int,
        purpose: str,
    ) -> Dict[str, Any]:
        """Append one request's cost and return the written entry."""
        amount = cost(model, input_tokens, output_tokens)
        entry = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "provider": provider,
            "model": model,
            "input_tokens": int(input_tokens),
            "output_tokens": int(output_tokens),
            "cost": round(amount, 10),
            "purpose": purpose,
        }
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, ensure_ascii=False) + "\n")
        return entry

    def spent(self, provider: str) -> float:
        return sum(
            float(entry.get("cost", 0.0))
            for entry in self.entries()
            if entry.get("provider") == provider
        )

    def totals(self) -> Dict[str, float]:
        """Spent USD per provider present in the ledger."""
        totals: Dict[str, float] = {}
        for entry in self.entries():
            provider = str(entry.get("provider", "unknown"))
            totals[provider] = totals.get(provider, 0.0) + float(entry.get("cost", 0.0))
        return totals

    def budget(self, provider: str) -> float:
        env_name = BUDGET_ENV.get(provider)
        if env_name is not None:
            raw = os.environ.get(env_name)
            if raw is not None and raw.strip():
                try:
                    return float(raw)
                except ValueError as error:
                    raise ValueError(f"{env_name} must be a number, got {raw!r}") from error
        try:
            return DEFAULT_BUDGETS[provider]
        except KeyError as error:
            raise ValueError(f"no budget for provider {provider!r}") from error

    def ensure(self, provider: str, estimate: float) -> None:
        """Refuse the next paid call when ``spent + estimate`` crosses the budget."""
        if estimate < 0:
            raise ValueError(f"estimate must not be negative, got {estimate}")
        current = self.spent(provider)
        limit = self.budget(provider)
        if current + estimate > limit:
            raise BudgetExceeded(
                f"{provider} budget would be exceeded: spent ${current:.4f} + "
                f"estimate ${estimate:.4f} > ${limit:.2f}"
            )


__all__ = [
    "BUDGET_ENV",
    "BudgetExceeded",
    "DEFAULT_BUDGETS",
    "Ledger",
    "PRICES",
    "cost",
    "estimate_cost",
    "price",
]
