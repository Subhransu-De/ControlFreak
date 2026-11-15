mod handoff;
mod smoke;

use crate::Suite;

pub fn all() -> Vec<Suite> {
    vec![smoke::suite(), handoff::suite()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;
    use std::collections::HashSet;

    #[test]
    fn suite_and_task_names_are_unique_and_scenes_exist() {
        let suites = all();
        let mut suite_names = HashSet::new();
        for suite in &suites {
            assert!(suite_names.insert(suite.name), "suite {} twice", suite.name);
            assert!(!suite.tasks.is_empty(), "suite {} is empty", suite.name);
            let mut task_names = HashSet::new();
            for task in &suite.tasks {
                assert!(
                    task_names.insert(task.name),
                    "{}/{} twice",
                    suite.name,
                    task.name
                );
                if let Some(scene) = task.scene {
                    Scene::builtin(scene)
                        .unwrap_or_else(|error| panic!("{}/{}: {error}", suite.name, task.name));
                }
            }
        }
    }
}
