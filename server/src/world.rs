// SPDX-FileCopyrightText: 2023 Softbear, Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::TowerService;
use common::alerts::{AlertFlag, Alerts};
use common::chunk::{ChunkId, ChunkInput, ChunkMaintenance, RelativeTowerId};
use common::force::Path;
use common::info::InfoEvent;
use common::player::{PlayerInput, PlayerMaintainance};
use common::ticks::Ticks;
use common::tower::{TowerId, TowerSet, TowerType};
use common::world::{World, WorldChunks};
use common_util::x_vec2::U16Vec2;
use core_protocol::id::PlayerId;
use fxhash::{FxHashMap, FxHashSet};
use game_server::player::PlayerRepo;
use glam::IVec2;
use rand::thread_rng;
use std::collections::VecDeque;
use std::time::Instant;

impl TowerService {
    pub fn spawn_player(
        &mut self,
        player_id: PlayerId,
        players: &PlayerRepo<Self>,
    ) -> Result<(), &'static str> {
        const MAX_TRIES: u32 = 100_000;

        let mut player = match players.borrow_player_mut(player_id) {
            Some(player) => player,
            None => return Err("player not in game"),
        };

        if player.alive {
            return Err("already alive");
        }

        let mut governor = MAX_TRIES;
        let start = Instant::now();

        // In towers^2.
        let search_area = 100u32;

        // In towers.
        let mut search_radius = (search_area as f32 * (1.0 / std::f32::consts::PI)).sqrt() as u16;

        let mut rng = thread_rng();
        let result = loop {
            if governor == 0 {
                println!(
                    "ran out of spawning attempts after {:?} (sr = {})",
                    start.elapsed(),
                    search_radius,
                );
                break Err("couldn't find spawnable tower");
            }
            governor -= 1;

            let tower_id = TowerId(
                U16Vec2::try_from(
                    (common_util::range::gen_radius(&mut rng, search_radius as f32)
                        + World::CENTER.0.as_vec2()
                        + 0.5)
                        .floor()
                        .as_ivec2()
                        .clamp(IVec2::ZERO, IVec2::splat(WorldChunks::SIZE as i32 - 1))
                        .as_uvec2(),
                )
                .unwrap(),
            );

            if self.is_spawnable(tower_id) {
                println!(
                    "took {} tries (sr = {:.2}) over {:?} to spawn {}",
                    MAX_TRIES - governor,
                    search_radius as f32 * TowerId::CONVERSION as f32,
                    start.elapsed(),
                    if player.is_bot() { "bot" } else { "player" }
                );

                player.lifetime = Ticks::ZERO;
                player.death_reason = None;
                player.score = 0;
                player.alerts = Alerts::default();

                break Ok(tower_id);
            }

            // TODO increase slower once very big.
            if governor % 8 == 0 {
                search_radius += 1;
            }
        };

        drop(player);

        let mut on_info_event = Self::on_info_event(players, |player_id| {
            debug_assert!(
                false,
                "spawning/increasing radius should not have killed player {:?}",
                player_id
            );
        });

        let tower_id = result?;
        {
            // Need to generate spawn point and it's neighbors.
            let mut tower_ids = FxHashSet::default();
            spawn_bubble(tower_id, player_id, |tower_id| {
                self.traverse(&mut tower_ids, tower_id)
            });
            self.generate(tower_ids, &mut on_info_event);

            // TODO optimization: save bubble in player and increment global tower refcount.

            let (chunk_id, tower_id) = tower_id.split();
            self.world.dispatch_chunk_input(
                chunk_id,
                ChunkInput::Spawn {
                    tower_id,
                    player_id,
                },
                &mut on_info_event,
            );
        }

        for tower_id in tower_id.neighbors() {
            let (chunk_id, tower_id) = tower_id.split();
            self.world.dispatch_chunk_input(
                chunk_id,
                ChunkInput::ClearZombies { tower_id },
                &mut on_info_event,
            );
        }
        Ok(())
    }

    pub fn alliance(
        &mut self,
        player_id: PlayerId,
        with: PlayerId,
        break_alliance: bool,
        players: &PlayerRepo<Self>,
    ) -> Result<(), &'static str> {
        // TODO visible to player?
        let Some(mut player) = players.borrow_player_mut(player_id) else {
            return Err("non-existent player");
        };
        let _a = &mut player.alerts;
        // TODO Add AlertFlag::MadeAlliance.
        drop(player);

        if !(self.regulator.active(player_id) && self.regulator.active(with)) {
            return Err("alliance with inactive player");
        }

        let new_alliance = !break_alliance
            && !self.world.player(player_id).allies.contains(&with)
            && self.world.player(with).allies.contains(&player_id);

        if new_alliance {
            for (a, b) in [(player_id, with), (with, player_id)] {
                self.world.dispatch_player_input(
                    a,
                    PlayerInput::NewAlliance(b),
                    Self::on_info_event(players, |_| unreachable!()),
                );
            }
        }

        for (a, b) in [(player_id, with), (with, player_id)] {
            let input = if break_alliance {
                PlayerInput::RemoveAlly(b)
            } else {
                PlayerInput::AddAlly(b)
            };
            self.world.dispatch_player_input(
                a,
                input,
                Self::on_info_event(players, |_| unreachable!()),
            );

            if !break_alliance {
                // Only breaking alliances is mutual.
                break;
            }
        }

        Ok(())
    }

    pub fn deploy_force(
        &mut self,
        player_id: PlayerId,
        tower_id: TowerId,
        path: Path,
        players: &PlayerRepo<Self>,
    ) -> Result<(), &'static str> {
        let tower = self.world.chunk.get(tower_id).ok_or("no tower")?;
        if tower.player_id != Some(player_id) {
            return Err("source not under player's control");
        }

        let strength = tower.force_units();
        if strength.is_empty() {
            return Err("empty force");
        }

        // Always some since strength isn't empty.
        let max_edge_distance = strength.max_edge_distance();
        let path = path.validate(&self.world.chunk, tower_id, max_edge_distance)?;

        if !player_id.is_bot() {
            let mut player = players.borrow_player_mut(player_id).ok_or_else(|| {
                debug_assert!(false, "missing player in deploy force");
                "missing player in deploy force"
            })?;
            let a = &mut player.alerts;
            a.set_flags(a.flags() | AlertFlag::DeployedAnyForce);
        }

        let (chunk_id, tower_id) = tower_id.split();
        self.world.dispatch_chunk_input(
            chunk_id,
            ChunkInput::DeployForce { tower_id, path },
            Self::on_info_event(players, |player_id| {
                debug_assert!(
                    false,
                    "deploying force should not have killed player {:?}",
                    player_id
                );
            }),
        );

        Ok(())
    }

    pub fn set_supply_line(
        &mut self,
        player_id: PlayerId,
        tower_id: TowerId,
        path: Option<Path>,
        players: &PlayerRepo<Self>,
    ) -> Result<(), &'static str> {
        let tower = self.world.chunk.get(tower_id).ok_or("no tower")?;
        if tower.player_id != Some(player_id) {
            return Err("source not under player's control");
        }

        if !tower.generates_mobile_units() {
            return Err("invalid supply line");
        }

        let max_edge_distance = tower.tower_type.ranged_distance();
        let path = path
            .map(|p| p.validate(&self.world.chunk, tower_id, max_edge_distance))
            .transpose()?
            .filter(|p| Some(p) != tower.supply_line.as_ref());

        if !player_id.is_bot() {
            let mut player = players.borrow_player_mut(player_id).ok_or_else(|| {
                debug_assert!(false, "missing player in set supply line");
                "missing player in set supply line"
            })?;

            let a = &mut player.alerts;
            a.set_flags(
                a.flags()
                    | if path.is_some() {
                        AlertFlag::SetAnySupplyLine
                    } else {
                        AlertFlag::UnsetAnySupplyLine
                    },
            );
        }

        let (chunk_id, tower_id) = tower_id.split();
        self.world.dispatch_chunk_input(
            chunk_id,
            ChunkInput::SetSupplyLine { tower_id, path },
            |info| {
                debug_assert!(false, "expected no info: {info:?}");
            },
        );

        Ok(())
    }

    /// Upgrade or downgrade tower.
    pub fn upgrade_tower(
        &mut self,
        player_id: PlayerId,
        tower_id: TowerId,
        upgrade: TowerType,
        players: &PlayerRepo<Self>,
    ) -> Result<(), &'static str> {
        let tower = match self.world.chunk.get(tower_id) {
            Some(tower) => tower,
            None => return Err("cannot upgrade nonexistent tower"),
        };

        if tower.player_id != Some(player_id) {
            return Err("cannot upgrade tower not owned");
        }

        if !tower.active() {
            return Err("upgrade already pending");
        }

        let Some(mut player) = players.borrow_player_mut(player_id) else {
            debug_assert!(false, "nonexistent player in upgrade tower");
            return Err("nonexistent player");
        };

        // Allow upgrade or downgrade.
        if tower.tower_type.can_upgrade_to(upgrade) {
            if !upgrade.has_prerequisites(&player.tower_counts) {
                return Err("missing prerequisite");
            }
            let a = &mut player.alerts;
            a.set_flags(a.flags() | AlertFlag::UpgradedAnyTower);
        } else if tower.tower_type.basis() != upgrade {
            return Err("invalid upgrade path");
        }

        drop(player);

        let (chunk_id, tower_id) = tower_id.split();
        self.world.dispatch_chunk_input(
            chunk_id,
            ChunkInput::UpgradeTower {
                tower_id,
                tower_type: upgrade,
            },
            Self::on_info_event(players, |player_id| {
                debug_assert!(
                    false,
                    "upgrading tower should not have killed player {:?}",
                    player_id
                );
            }),
        );

        Ok(())
    }

    /// # Panics
    ///
    /// If player wasn't passed in and doesn't exist.
    pub fn kill_player(&mut self, player_id: PlayerId, players: &PlayerRepo<Self>) {
        let mut player = players.borrow_player_mut(player_id).unwrap();
        player.alive = false;
        drop(player);

        let mut on_info = Self::on_info_event(players, |player_id| {
            debug_assert!(
                false,
                "player {:?} is already dead, should not be killable",
                player_id
            );
        });

        let chunk_ids: Vec<_> = self.world.chunk.iter_chunks().map(|(id, _)| id).collect();
        for chunk_id in chunk_ids {
            self.world.dispatch_chunk_maintenance(
                chunk_id,
                ChunkMaintenance::KillPlayer { player_id },
                &mut on_info,
            );
        }

        // Note: the player may not exist in the actor model if player dies one tick after
        // leaving, causing the regulator to remove the actor in the tick they die. This
        // function is called one tick after that.
        if self.world.player.contains_key(&player_id) {
            for ally_id in self
                .world
                .player(player_id)
                .allies
                .iter()
                .copied()
                .collect::<Vec<_>>()
            {
                if !self.world.player.contains_key(&ally_id) {
                    continue;
                } else {
                    // TODO.
                }
                self.world.dispatch_player_maintenance(
                    ally_id,
                    PlayerMaintainance::RemoveDeadAlly(player_id),
                    &mut on_info,
                );
            }
            self.world.dispatch_player_maintenance(
                player_id,
                PlayerMaintainance::Died,
                &mut on_info,
            );
        }

        debug_assert_eq!(
            players.borrow_player(player_id).unwrap().towers,
            FxHashSet::default()
        );
    }

    /// Removes towers if there are too many.
    pub fn shrink(&mut self, players: &PlayerRepo<Self>) {
        // TODO don't allocate max world size.
        let mut locked = TowerSet::with_bounds(WorldChunks::RECTANGLE);
        for (tower_id, tower) in self.world.chunk.iter_towers() {
            if !tower.can_destroy() {
                let mut t = tower_id;
                while locked.insert(t) {
                    if let Some(connectivity_id) = t.connectivity_id() {
                        t = connectivity_id;
                    } else {
                        break;
                    }
                }

                // Lock towers around king within spawn_bubble (if they exist).
                for player_id in tower.iter_rulers() {
                    spawn_bubble(tower_id, player_id, |mut t| {
                        if self.world.chunk.contains(t) {
                            while locked.insert(t) {
                                if let Some(connectivity_id) = t.connectivity_id() {
                                    t = connectivity_id;
                                } else {
                                    break;
                                }
                            }
                        }
                    })
                }
            }
        }

        let mut destroy = vec![];
        for (tower_id, tower) in self.world.chunk.iter_towers() {
            if !locked.contains(tower_id) {
                debug_assert!(tower.can_destroy());
                destroy.push(tower_id);
            }
        }

        self.destroy(
            destroy,
            &mut Self::on_info_event(players, |_| unreachable!("generate killed player")),
        )
    }

    pub fn is_spawnable(&self, tower_id: TowerId) -> bool {
        tower_id.connectivity().is_some()
            && self.is_good_spawn(tower_id)
            && self.is_safe_spawn(tower_id)
    }

    fn is_good_spawn(&self, tower_id: TowerId) -> bool {
        let get_tower_type = |tower_id: TowerId| -> Result<TowerType, ()> {
            Ok(if let Some(tower) = self.world.chunk.get(tower_id) {
                if tower.player_id.is_some() {
                    return Err(());
                }
                tower.tower_type
            } else {
                tower_id.tower_type()
            })
        };
        let Ok(tower_type) = get_tower_type(tower_id) else {
            return false;
        };

        let mut neighbors = 0;
        let mut spawnable_neighbors = 0;
        for neighbor_id in tower_id.neighbors() {
            neighbors += 1;
            let Ok(neighbor_tower_type) = get_tower_type(neighbor_id) else {
                return false;
            };

            if neighbor_tower_type.is_spawnable() {
                spawnable_neighbors += 1;
            }
        }
        (tower_type.is_spawnable() && neighbors >= 3) || spawnable_neighbors >= 2
    }

    fn is_safe_spawn(&self, tower_id: TowerId) -> bool {
        let mut set = 0u64;
        let mut insert = |id: TowerId| -> bool {
            let index = (id.x & 0b111) | ((id.y & 0b111) << 3);
            let bit: u64 = 1u64 << index;
            let inserted = set & bit == 0;
            set |= bit;
            inserted
        };
        let mut a = &mut [TowerId::default(); 16];
        let mut b = &mut [TowerId::default(); 16];

        a[0] = tower_id;
        let mut len = 1;

        for _ in 0..4 {
            for tower_id in &a[..std::mem::take(&mut len).min(a.len())] {
                for tower_id in tower_id.neighbors() {
                    if !insert(tower_id) {
                        continue;
                    }
                    if self
                        .world
                        .chunk
                        .get(tower_id)
                        .is_some_and(|t| t.player_id.is_some() || !t.inbound_forces.is_empty())
                    {
                        return false;
                    }
                    // no panic.
                    b[len % b.len()] = tower_id;
                    len += 1;
                }
            }

            std::mem::swap(&mut a, &mut b);
        }
        /*
        println!("sus {len}");
        for b in set.to_le_bytes() {
            println!("{b:08b}");
        }
        */
        let ret = set.count_ones() >= 12;
        //println!("enough = {ret}");
        ret
    }

    /// Adds to `tower_ids` along the path towards the center from `tower_id`.
    fn traverse(&self, tower_ids: &mut FxHashSet<TowerId>, mut tower_id: TowerId) {
        while self.world.chunk.get(tower_id).is_none() {
            if !tower_ids.insert(tower_id) {
                break;
            }
            if let Some(connectivity) = tower_id.connectivity() {
                tower_id = tower_id.neighbor_unchecked(connectivity);
            } else {
                break;
            }
        }
    }

    /// Destroys all the `tower_ids`.
    fn destroy(
        &mut self,
        tower_ids: impl IntoIterator<Item = TowerId>,
        c: &mut impl FnMut(InfoEvent),
    ) {
        for (chunk_id, tower_ids) in group(tower_ids) {
            let input = ChunkMaintenance::Destroy { tower_ids };
            self.world
                .dispatch_chunk_maintenance(chunk_id, input, &mut *c);
        }
    }

    /// Generates all the `tower_ids`.
    fn generate(
        &mut self,
        tower_ids: impl IntoIterator<Item = TowerId>,
        c: &mut impl FnMut(InfoEvent),
    ) {
        for (chunk_id, tower_ids) in group(tower_ids) {
            let input = ChunkInput::Generate { tower_ids };
            self.world.dispatch_chunk_input(chunk_id, input, &mut *c);
        }
    }
}

/// Calls `f` for ever tower in `player_id`'s spawn bubble around `f`.
fn spawn_bubble(tower_id: TowerId, player_id: PlayerId, f: impl FnMut(TowerId)) {
    let radius = if player_id.is_bot() && !cfg!(debug_assertions) {
        35
    } else {
        50
    };
    bubble(tower_id, radius, f)
}

/// Calls `f` for ever [`TowerId`] within `radius` of `origin`.
fn bubble(origin: TowerId, radius: u16, mut f: impl FnMut(TowerId)) {
    // TODO TowerSet with radius sized allocation.
    let mut seen = FxHashSet::default();
    let mut queue = VecDeque::new();
    let r2 = (radius as u64).pow(2);

    seen.insert(origin);
    queue.push_back(origin);
    f(origin);

    while let Some(tower_id) = queue.pop_front() {
        for tower_id in tower_id.neighbors() {
            if tower_id.distance_squared(origin) > r2 {
                continue;
            }

            if seen.insert(tower_id) {
                queue.push_back(tower_id);
                f(tower_id);
            }
        }
    }
}

/// Groups `tower_ids` into [`ChunkId`]s and [`RelativeTowerId`]s.
fn group(tower_ids: impl IntoIterator<Item = TowerId>) -> FxHashMap<ChunkId, Vec<RelativeTowerId>> {
    let mut chunk_map: FxHashMap<ChunkId, Vec<RelativeTowerId>> = Default::default();
    for tower_id in tower_ids {
        let (chunk_id, tower_id) = tower_id.split();
        chunk_map.entry(chunk_id).or_default().push(tower_id)
    }
    chunk_map
}
