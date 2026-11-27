"""Check catalog indexing, static component pins and operation vocabulary."""
import hashlib
import json
import re
from jsonschema import Draft202012Validator
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent / "v2"


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"Duplicate JSON field: {key}")
        result[key] = value
    return result


def validate():
    catalog = json.loads((ROOT / "catalog.json").read_bytes(), object_pairs_hook=unique_fields)
    for name in ("catalog", "recipe"):
        schema = json.loads((ROOT / "schemas" / (name + ".json")).read_text())
        Draft202012Validator.check_schema(schema)
        if name == "catalog":
            Draft202012Validator(schema).validate(catalog)
        else:
            recipe_schema = Draft202012Validator(schema)
    if catalog["schema"] != "StoreCatalogV1":
        raise ValueError("unsupported catalog")
    seen = set()
    for entry in catalog["recipes"]:
        identity = entry["id"]
        if not re.fullmatch(r"[a-z0-9]+(?:[.-][a-z0-9]+)*", identity) or identity in seen:
            raise ValueError("invalid or duplicate recipe identifier")
        seen.add(identity)
        data = (ROOT / "recipes" / (identity + ".json")).read_bytes()
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise ValueError(f"recipe digest mismatch for {identity}")
        recipe = json.loads(data, object_pairs_hook=unique_fields)
        recipe_schema.validate(recipe)
        if recipe["schema"] != "StoreRecipeV2" or recipe["id"] != identity:
            raise ValueError("recipe identity mismatch")
        for item in recipe["components"]:
            if (not re.fullmatch(r"[0-9a-f]{64}", item["sha256"])
                    or not 0 < item["bytes"] <= 8 * 1024**3
                    or urlsplit(item["url"]).scheme != "https"
                    or item["format"] not in {"7z", "iso9660", "tar.bz2", "zip", "raw", "dmg-hfs"}):
                raise ValueError(f"invalid component in {identity}")
        components = {c["name"] for c in recipe["components"]}
        targets = [b["platform"] for b in recipe["builds"]]
        if len(set(targets)) != len(targets) or components != {c for b in recipe["builds"] for c in b["components"]}:
            raise ValueError("duplicate platform or inconsistent component references")
    if seen != {p.stem for p in (ROOT / "recipes").glob("*.json")}:
        raise ValueError("unindexed recipe")


if __name__ == "__main__":
    validate()
