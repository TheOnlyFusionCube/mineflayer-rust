//! The crafting recipes of the vanilla server: `vanilla_recipes.json`,
//! written by `scripts/gen-recipes.py` from the server jar at each version
//! upgrade (`docs/version_upgrade.md`).

use std::sync::LazyLock;

use serde::Deserialize;

use super::*;

/// One ingredient: an item id, a `#tag`, or a list of ids.
#[derive(Deserialize)]
#[serde(untagged)]
enum JsonIngredient {
    One(String),
    Any(Vec<String>),
}

/// One recipe as the generator writes it.
#[derive(Deserialize)]
struct JsonRecipe {
    r: String,
    c: i32,
    s: Option<Vec<Vec<Option<JsonIngredient>>>>,
    i: Option<Vec<JsonIngredient>>,
}

/// Every shaped and shapeless crafting recipe of the vanilla server.
pub(super) static RECIPES: LazyLock<Vec<Recipe>> = LazyLock::new(|| {
    serde_json::from_str::<Vec<JsonRecipe>>(include_str!("vanilla_recipes.json"))
        .expect("the generated recipe table parses")
        .into_iter()
        .filter_map(recipe)
        .collect()
});

/// `None` when an id or tag is not in this engine's registry.
fn recipe(json: JsonRecipe) -> Option<Recipe> {
    let result = RecipeResult {
        kind: json.r.parse().ok()?,
        count: json.c,
        metadata: None,
    };
    let shape = match json.s {
        Some(rows) => Some(
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| match cell {
                            // an empty cell
                            None => Some(None),
                            Some(json) => ingredient(json).map(Some),
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        None => None,
    };
    let ingredients = match json.i {
        Some(list) => Some(list.iter().map(ingredient).collect::<Option<Vec<_>>>()?),
        None => None,
    };
    Some(Recipe::new(result, shape, ingredients))
}

fn ingredient(json: &JsonIngredient) -> Option<Ingredient> {
    let ids: Vec<&str> = match json {
        JsonIngredient::One(id) => vec![id],
        JsonIngredient::Any(ids) => ids.iter().map(String::as_str).collect(),
    };
    let mut any_of = Vec::new();
    for id in ids {
        match id.strip_prefix('#') {
            Some(tag) => any_of.extend(item_tag_by_name(tag)?.iter().map(|kind| RecipeItem {
                kind: *kind,
                metadata: None,
            })),
            None => any_of.push(RecipeItem {
                kind: id.parse().ok()?,
                metadata: None,
            }),
        }
    }
    (!any_of.is_empty()).then_some(Ingredient { any_of })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_every_crafting_recipe() {
        assert_eq!(RECIPES.len(), 1030, "every generated recipe resolves");
        let sticks: Vec<_> = RECIPES
            .iter()
            .filter(|r| r.result.kind == ItemKind::Stick)
            .collect();
        assert!(!sticks.is_empty());
        // the planks tag expands to every plank
        let planks = &sticks[0].in_shape.as_ref().unwrap()[0][0]
            .as_ref()
            .unwrap()
            .any_of;
        assert!(planks.iter().any(|i| i.kind == ItemKind::OakPlanks));
        assert!(planks.iter().any(|i| i.kind == ItemKind::BambooPlanks));
        let table = RECIPES
            .iter()
            .find(|r| r.result.kind == ItemKind::CraftingTable)
            .unwrap();
        assert!(!table.requires_table, "a 2x2 recipe");
        let chest = RECIPES
            .iter()
            .find(|r| r.result.kind == ItemKind::Chest)
            .unwrap();
        assert!(chest.requires_table);
    }

    /// The book lists a tag's items in another order than the table; the
    /// merge in `recipes_for` must still see one recipe.
    #[test]
    fn a_reordered_recipe_is_the_same_recipe() {
        let stick = RECIPES
            .iter()
            .find(|r| r.result.kind == ItemKind::Stick)
            .unwrap();
        let mut shape = stick.in_shape.clone().unwrap();
        for row in &mut shape {
            for cell in row.iter_mut().flatten() {
                cell.any_of.reverse();
            }
        }
        let reordered = Recipe::new(stick.result, Some(shape), None);
        assert_ne!(&reordered, stick);
        assert_eq!(reordered.normalized(), stick.normalized());
    }
}
