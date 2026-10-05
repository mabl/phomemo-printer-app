#!/usr/bin/env python3

"""Generate a normalized Phomemo media catalog from reference JSON inputs.

This script transforms the media definitions bundled with the Print Master
app (``localPaper.json``, ``DefaultPrinter.json`` and
``DefaultTypeGroup.json`` under the APK's ``assets/``) into a clean,
driver-friendly catalog consumed by ``phomemo-protocol``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "phomemo-protocol" / "data" / "media_catalog.json"
GENERATOR = "scripts/generate_media_catalog.py"

LOCAL_PAPER = "localPaper.json"
DEFAULT_PRINTER = "DefaultPrinter.json"
DEFAULT_TYPE_GROUP = "DefaultTypeGroup.json"


PAPER_TYPE_TO_TRACKING = {
    0: "continuous",
    1: "gap",
    2: "gap",
    3: "mark",
    4: "card",
}

# localPaper.json series strings, in matching priority order: a series that
# is not listed verbatim goes to the first entry sharing one of its models.
LOCAL_SERIES_TO_POOL = {
    "M200": "M200",
    "M110/M120/M100": "M110",
    "D30/A30/P15": "D30",
    "D50": "D50",
    "P1000": "P1000",
    "P12/LT12/F12": "P12",
    "D480": "D480",
    "P780/D680": "P780",
    "P3100/P3200/D1600/M960/M950/LM1600": "P3100",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--reference-dir",
        type=Path,
        required=True,
        help=f"directory holding {LOCAL_PAPER}, {DEFAULT_PRINTER} and {DEFAULT_TYPE_GROUP}",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=DEFAULT_OUT,
        help=f"output JSON path (default: {DEFAULT_OUT.relative_to(REPO_ROOT)})",
    )
    return parser.parse_args()


def _load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def _describe_source(path: Path) -> dict[str, str]:
    """Identify an input by name and content, without its local location."""
    return {"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def _fmt_mm(value: float) -> str:
    if float(value).is_integer():
        return str(int(value))
    text = f"{value:.3f}".rstrip("0").rstrip(".")
    return text.replace(".", "p")


def _size_name(width_mm: float, length_mm: float) -> str:
    width = _fmt_mm(width_mm)
    length = _fmt_mm(length_mm)
    return f"om_{width}x{length}mm_{width}x{length}mm"


def _paper_type_from_raw(
    raw: str, entry_id: int, anomalies: list[dict[str, Any]]
) -> tuple[int, str]:
    if re.fullmatch(r"-?\d+", raw):
        return int(raw), "exact"
    match = re.search(r"-?\d+", raw)
    if match is None:
        raise ValueError(f"entry id {entry_id}: paperType is not numeric: {raw!r}")

    parsed = int(match.group())
    anomalies.append(
        {
            "kind": "paper_type_coercion",
            "id": entry_id,
            "raw": raw,
            "parsed": parsed,
            "reason": "non-digit characters present",
        }
    )
    return parsed, "coerced"


def _material_name(raw: str) -> str:
    mapping = {
        "白色": "white",
        "透明": "transparent",
    }
    return mapping.get(raw, "unknown")


def _series_models(series: str) -> set[str]:
    return set(filter(None, series.split("/")))


def _pool_from_series(series: str, anomalies: list[dict[str, Any]]) -> str:
    if series in LOCAL_SERIES_TO_POOL:
        return LOCAL_SERIES_TO_POOL[series]

    models = _series_models(series)
    for known_series, pool in LOCAL_SERIES_TO_POOL.items():
        if models & _series_models(known_series):
            return pool

    anomalies.append(
        {
            "kind": "unknown_local_series_bucket",
            "series": series,
            "fallback_pool": "M110",
        }
    )
    return "M110"


def _resolve_pool_for_series(series: str, anomalies: list[dict[str, Any]]) -> str:
    # Mirrors Print Master's LocalPaperPresenter routing (mLocalPapers,
    # dLocalPapers and pLocalPapers).
    if series == "M200":
        return "M200"
    if series in {"M110", "M120", "M100", "M150", "M400", "E600S", "E9000", "E50"}:
        return "M110"

    if series == "D50":
        return "D50"
    if series in {"P12", "F12", "LT12"}:
        return "P12"
    if series in {"D30", "Q30", "A30", "P15", "DM170"}:
        return "D30"

    if series == "P1000":
        return "P1000"
    if series == "D480":
        return "D480"
    if series in {"P780", "D680"}:
        return "P780"
    if series in {"P3100", "P3100D", "P3200", "D1600", "M960", "M950", "LM1600"}:
        return "P3100"

    if series.startswith(("M", "E")):
        anomalies.append(
            {
                "kind": "series_fallback",
                "series": series,
                "fallback_pool": "M110",
                "reason": "M/E family fallback from PM mLocalPapers default",
            }
        )
        return "M110"
    if series.startswith(("D", "Q", "A")) or series in {"P15", "DM170"}:
        anomalies.append(
            {
                "kind": "series_fallback",
                "series": series,
                "fallback_pool": "D30",
                "reason": "D-family fallback from PM dLocalPapers default",
            }
        )
        return "D30"
    if series.startswith("P") or series in {"LT12", "LM1600"}:
        anomalies.append(
            {
                "kind": "series_fallback",
                "series": series,
                "fallback_pool": "P3100",
                "reason": "P-family fallback from PM pLocalPapers default",
            }
        )
        return "P3100"

    anomalies.append(
        {
            "kind": "series_unknown",
            "series": series,
            "fallback_pool": "M110",
        }
    )
    return "M110"


def _pick_default_size(media: list[dict[str, Any]]) -> str:
    by_name = {m["size_name"]: m for m in media}

    for preferred_name in (
        "om_40x30mm_40x30mm",
        "om_50x30mm_50x30mm",
        "om_30x15mm_30x15mm",
    ):
        candidate = by_name.get(preferred_name)
        if candidate is None:
            continue
        if candidate["tracking_default"] in {"gap", "continuous"}:
            return preferred_name

    ranking = {"gap": 0, "continuous": 1, "mark": 2, "card": 3}
    ordered = sorted(
        media,
        key=lambda m: (
            ranking.get(m["tracking_default"], 9),
            abs(m["width_mm"] - 40.0) + abs(m["length_mm"] - 30.0),
            m["width_mm"],
            m["length_mm"],
        ),
    )
    return ordered[0]["size_name"]


def _build_pools(
    local_paper: list[dict[str, str]], anomalies: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    pools: dict[str, dict[str, Any]] = {}

    for raw in local_paper:
        entry_id = int(raw["id"])
        group_id = int(raw["groupId"])
        width_mm = float(raw["width"])
        length_mm = float(raw["height"])
        paper_type_id, paper_type_parse = _paper_type_from_raw(
            raw["paperType"], entry_id, anomalies
        )

        if paper_type_id not in PAPER_TYPE_TO_TRACKING:
            anomalies.append(
                {
                    "kind": "unknown_paper_type",
                    "id": entry_id,
                    "paper_type_id": paper_type_id,
                }
            )
            continue

        tracking = PAPER_TYPE_TO_TRACKING[paper_type_id]
        pool = _pool_from_series(raw["series"], anomalies)

        pool_obj = pools.setdefault(
            pool,
            {
                "pool": pool,
                "source_series": set(),
                "aliases": set(),
                "_media": {},
            },
        )
        pool_obj["source_series"].add(raw["series"])
        for alias in raw["series"].split("/"):
            if alias:
                pool_obj["aliases"].add(alias)

        media_key = (width_mm, length_mm)
        media_obj = pool_obj["_media"].setdefault(
            media_key,
            {
                "size_name": _size_name(width_mm, length_mm),
                "width_mm": width_mm,
                "length_mm": length_mm,
                "origin": "pm_local_paper",
                "tracking_supported": set(),
                "paper_type_ids": set(),
                "materials": set(),
                "offsets_mm": {
                    "left": float(raw["leftOffset"]),
                    "right": float(raw["rightOffset"]),
                    "top": float(raw["topOffset"]),
                    "bottom": float(raw["bottomOffset"]),
                },
                "source_ids": [],
                "source_group_ids": set(),
                "normalization": set(),
            },
        )

        media_obj["tracking_supported"].add(tracking)
        media_obj["paper_type_ids"].add(paper_type_id)
        media_obj["materials"].add(_material_name(raw["name"]))
        media_obj["source_ids"].append(entry_id)
        media_obj["source_group_ids"].add(group_id)
        media_obj["normalization"].add(paper_type_parse)

    # Add synthesized continuous-roll presets for every known width so
    # PAPPL/CUPS can expose endless media across all pools.
    for pool_name, pool_obj in pools.items():
        media_by_key = pool_obj["_media"]
        widths = sorted(
            {media["width_mm"] for media in media_by_key.values() if media["width_mm"] > 0}
        )
        width_materials: dict[float, set[str]] = defaultdict(set)
        for media in media_by_key.values():
            if media["width_mm"] > 0:
                width_materials[media["width_mm"]].update(media["materials"])

        for width_mm in widths:
            roll_key = (width_mm, 0.0)
            if roll_key in media_by_key:
                continue

            materials = width_materials.get(width_mm, set())
            media_by_key[roll_key] = {
                "size_name": _size_name(width_mm, 0.0),
                "width_mm": width_mm,
                "length_mm": 0.0,
                "origin": "driver_synthetic_continuous_roll",
                "tracking_supported": {"continuous"},
                "paper_type_ids": {0},
                "materials": set(materials),
                "offsets_mm": {
                    "left": 0.0,
                    "right": 0.0,
                    "top": 0.0,
                    "bottom": 0.0,
                },
                "source_ids": [],
                "source_group_ids": set(),
                "normalization": {"synthetic"},
            }
            anomalies.append(
                {
                    "kind": "synthetic_continuous_roll",
                    "pool": pool_name,
                    "width_mm": width_mm,
                }
            )

    rendered: list[dict[str, Any]] = []
    for pool in sorted(pools):
        p = pools[pool]
        media_items: list[dict[str, Any]] = []
        for _key, media in sorted(
            p["_media"].items(),
            key=lambda item: (item[1]["width_mm"], item[1]["length_mm"]),
        ):
            tracking_supported = sorted(media["tracking_supported"])
            if "gap" in tracking_supported:
                tracking_default = "gap"
            elif "continuous" in tracking_supported:
                tracking_default = "continuous"
            elif "mark" in tracking_supported:
                tracking_default = "mark"
            else:
                tracking_default = tracking_supported[0]

            media_items.append(
                {
                    "size_name": media["size_name"],
                    "width_mm": media["width_mm"],
                    "length_mm": media["length_mm"],
                    "origin": media["origin"],
                    "tracking_supported": tracking_supported,
                    "tracking_default": tracking_default,
                    "paper_type_ids": sorted(media["paper_type_ids"]),
                    "materials": sorted(media["materials"]),
                    "offsets_mm": media["offsets_mm"],
                    "source_ids": sorted(media["source_ids"]),
                    "source_group_ids": sorted(media["source_group_ids"]),
                    "normalization": sorted(media["normalization"]),
                }
            )

        rendered.append(
            {
                "pool": pool,
                "source_series": sorted(p["source_series"]),
                "aliases": sorted(p["aliases"]),
                "default_size_name": _pick_default_size(media_items),
                "media": media_items,
            }
        )

    return rendered


def _collect_types(
    default_printer: list[dict[str, Any]], default_type_group: dict[str, Any]
) -> dict[str, dict[str, Any]]:
    gathered: dict[str, dict[str, Any]] = {}

    def register(
        printer_type: str, series: str | None, sn: str | None, display_name: str | None, source: str
    ) -> None:
        if not printer_type:
            return
        entry = gathered.setdefault(
            printer_type,
            {
                "series_counts": Counter(),
                "sn_prefixes": set(),
                "display_names": set(),
                "sources": set(),
            },
        )
        if series:
            entry["series_counts"][series] += 1
        if sn:
            entry["sn_prefixes"].add(sn)
        if display_name:
            entry["display_names"].add(display_name)
        entry["sources"].add(source)

    for row in default_printer:
        register(
            str(row.get("type", "")).strip(),
            str(row.get("series", "")).strip() or None,
            str(row.get("sn", "")).strip() or None,
            str(row.get("displayName", "")).strip() or None,
            DEFAULT_PRINTER,
        )

    groups = default_type_group.get("data", {}).get("list", [])
    for group in groups:
        for row in group.get("list", []):
            register(
                str(row.get("type", "")).strip(),
                str(row.get("series", "")).strip() or None,
                str(row.get("sn", "")).strip() or None,
                str(row.get("displayName", "")).strip() or None,
                DEFAULT_TYPE_GROUP,
            )

    return gathered


def _build_printer_types(
    type_rows: dict[str, dict[str, Any]],
    pools: list[dict[str, Any]],
    anomalies: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    pools_by_name = {pool["pool"]: pool for pool in pools}
    result: list[dict[str, Any]] = []

    for printer_type in sorted(type_rows):
        row = type_rows[printer_type]
        series_counts: Counter[str] = row["series_counts"]
        if series_counts:
            # Deterministic winner for conflicting series mappings.
            series = sorted(series_counts.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
            if len(series_counts) > 1:
                anomalies.append(
                    {
                        "kind": "type_series_conflict",
                        "type": printer_type,
                        "series_counts": dict(sorted(series_counts.items())),
                        "chosen_series": series,
                    }
                )
        else:
            series = printer_type
            anomalies.append(
                {
                    "kind": "type_missing_series",
                    "type": printer_type,
                    "fallback_series": series,
                }
            )

        pool = _resolve_pool_for_series(series, anomalies)
        if pool not in pools_by_name:
            anomalies.append(
                {
                    "kind": "pool_missing",
                    "type": printer_type,
                    "series": series,
                    "pool": pool,
                }
            )
            continue

        pool_obj = pools_by_name[pool]
        display_name = sorted(row["display_names"])[0] if row["display_names"] else printer_type
        result.append(
            {
                "type": printer_type,
                "display_name": display_name,
                "series": series,
                "pool": pool,
                "default_size_name": pool_obj["default_size_name"],
                "media_count": len(pool_obj["media"]),
                "sn_prefixes": sorted(row["sn_prefixes"]),
                "sources": sorted(row["sources"]),
            }
        )

    return result


def main() -> int:
    args = parse_args()
    local_paper_path = args.reference_dir / LOCAL_PAPER
    default_printer_path = args.reference_dir / DEFAULT_PRINTER
    default_type_group_path = args.reference_dir / DEFAULT_TYPE_GROUP
    out_path = args.out

    local_paper = _load_json(local_paper_path)
    default_printer = _load_json(default_printer_path)
    default_type_group = _load_json(default_type_group_path)

    anomalies: list[dict[str, Any]] = []
    pools = _build_pools(local_paper, anomalies)
    type_rows = _collect_types(default_printer, default_type_group)
    printer_types = _build_printer_types(type_rows, pools, anomalies)

    unique_anomalies: list[dict[str, Any]] = []
    seen_anomalies: set[str] = set()
    for anomaly in anomalies:
        key = json.dumps(anomaly, sort_keys=True, ensure_ascii=True)
        if key in seen_anomalies:
            continue
        seen_anomalies.add(key)
        unique_anomalies.append(anomaly)

    summary = {
        "local_paper_entries": len(local_paper),
        "paper_pools": len(pools),
        "printer_types": len(printer_types),
        "anomalies": len(unique_anomalies),
    }

    catalog = {
        "schema_version": 1,
        "generator": GENERATOR,
        "sources": {
            "local_paper": _describe_source(local_paper_path),
            "default_printer": _describe_source(default_printer_path),
            "default_type_group": _describe_source(default_type_group_path),
        },
        "summary": summary,
        "paper_pools": pools,
        "printer_types": printer_types,
        "anomalies": sorted(
            unique_anomalies,
            key=lambda row: (
                str(row.get("kind", "")),
                str(row.get("type", "")),
                str(row.get("series", "")),
                str(row.get("id", "")),
            ),
        ),
    }

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as f:
        json.dump(catalog, f, indent=2, sort_keys=False, ensure_ascii=True)
        f.write("\n")

    print(f"wrote {out_path}")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
