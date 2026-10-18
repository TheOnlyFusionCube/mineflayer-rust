#!/usr/bin/env python3
"""Write src/craft/vanilla_recipes.json, the crafting recipes of the vanilla
server, from the server jar the live fixture downloads.

Usage: python3 scripts/gen-recipes.py [.live/server.jar]

Only the shaped and shapeless crafting recipes go in: the others (smelting,
stonecutting, smithing, the special recipes) do not use the crafting grid.
Each entry is {"r": result id, "c": count, "s": shape rows} for a shaped
recipe, where a shape cell is an ingredient or null, or
{"r", "c", "i": ingredients} for a shapeless one. An ingredient is an item
id, a "#tag", or a list of ids. Ids lose their "minecraft:" prefix.
"""
import io
import json
import sys
import zipfile

jar_path = sys.argv[1] if len(sys.argv) > 1 else ".live/server.jar"
outer = zipfile.ZipFile(jar_path)
inner_name = next(n for n in outer.namelist() if n.startswith("META-INF/versions/") and n.endswith(".jar"))
inner = zipfile.ZipFile(io.BytesIO(outer.read(inner_name)))


def short(id_):
    return id_.removeprefix("minecraft:").replace("#minecraft:", "#")


def ingredient(value):
    if isinstance(value, list):
        return [short(v) for v in value]
    return short(value)


recipes = []
for name in sorted(inner.namelist()):
    if not (name.startswith("data/minecraft/recipe/") and name.endswith(".json")):
        continue
    data = json.loads(inner.read(name))
    result = data.get("result", {})
    entry = {"r": short(result.get("id", "")), "c": result.get("count", 1)}
    if data["type"] == "minecraft:crafting_shaped":
        key = data["key"]
        entry["s"] = [[None if c == " " else ingredient(key[c]) for c in row] for row in data["pattern"]]
    elif data["type"] == "minecraft:crafting_shapeless":
        entry["i"] = [ingredient(i) for i in data["ingredients"]]
    else:
        continue
    recipes.append(entry)

with open("src/craft/vanilla_recipes.json", "w") as out:
    json.dump(recipes, out, separators=(",", ":"))
    out.write("\n")
print(f"{len(recipes)} recipes from {inner_name}")
