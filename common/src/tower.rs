use crate::enum_array::EnumArray;
use crate::force::{Force, Path};
use crate::ticks::Ticks;
use crate::unit::Unit;
use crate::units::Units;
use core_protocol::id::PlayerId;
use core_protocol::prelude::*;
pub use id::TowerId;
use macros::TowerTypeData;
pub use map::TowerMap;
use num_enum::{IntoPrimitive, TryFromPrimitive};
pub use rectangle::TowerRectangle;
pub use set::TowerSet;
use std::num::NonZeroU8;
use strum::{Display, EnumIter, EnumString, IntoEnumIterator};

#[cfg(any(test, feature = "server"))]
mod connectivity;
mod id;
mod map;
mod rectangle;
mod set;

/// Deterministic, precisely rounded down, integer sqrt.
pub(crate) fn integer_sqrt(y: u64) -> u32 {
    use num_integer::Roots;
    y.sqrt() as u32 // sqrt(u64::MAX) == u32::MAX
}

/// Fast but imprecise integer sqrt. About twice as fast as [`integer_sqrt`].
/// Don't rely on this being deterministic.
///
/// # Panics
///
/// In debug mode, if y is too large (never panics when finding distance between two [`TowerId`]'s).
/// In release mode, returns 0.
#[allow(unused)] // TODO maybe remove?
pub(crate) fn fast_integer_sqrt(y: u64) -> u32 {
    unsafe {
        let v: u64 = (y as f64).sqrt().to_int_unchecked();
        debug_assert!(v < u32::MAX as u64); // fast sqrt u64::MAX > u32::MAX
        v as u32
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize, Encode, Decode)]
pub struct Tower {
    pub player_id: Option<PlayerId>,
    pub units: Units,
    pub tower_type: TowerType,
    /// Delay until usable in ticks. Currently used to implement upgrading.
    pub delay: Option<NonZeroU8>,
    /// These forces will eventually arrive and be processed.
    pub inbound_forces: Vec<Force>,
    /// Mirrors inbound forces of opposing tower. When they would arrive, they are discarded.
    pub outbound_forces: Vec<Force>,
    /// Where the tower will send its units when it can't generate or is overflowing.
    pub supply_line: Option<Path>,
}

impl Tower {
    pub const RULER_SHIELD_BOOST: usize = 10;

    pub fn new(tower_id: TowerId) -> Self {
        Self::with_type(tower_id.tower_type())
    }

    pub fn with_type(tower_type: TowerType) -> Self {
        Self {
            player_id: None,
            units: Units::default(),
            tower_type,
            delay: None,
            inbound_forces: Vec::new(),
            outbound_forces: Vec::new(),
            supply_line: None,
        }
    }

    /// Returns if the tower should provide it's actions besides moving forces.
    /// Inactive towers don't generate units, provide increased sensors, or count towards upgrades.
    pub fn active(&self) -> bool {
        self.delay.is_none()
    }

    /// Returns true if the [`Tower`] is eligible to be destroyed.
    pub fn can_destroy(&self) -> bool {
        self.inbound_forces.is_empty() && self.player_id.is_none()
    }

    /// Returns an iterator over [`PlayerId`]s with their ruler at or incoming to this tower.
    pub fn iter_rulers(&self) -> impl Iterator<Item = PlayerId> + '_ {
        self.player_id
            .filter(|_| self.units.has_ruler())
            .into_iter()
            .chain(
                self.inbound_forces
                    .iter()
                    .filter_map(|f| f.units.has_ruler().then(|| f.player_id.unwrap())),
            )
    }

    /// Gets all units that can be deployed in a force.
    pub fn force_units(&self) -> Units {
        let mut ret = Units::default();
        for (unit, count) in self.units.iter() {
            if !unit.is_mobile(Some(self.tower_type)) {
                continue;
            }
            ret.add(unit, count);
        }
        ret
    }

    /// Takes all units that can be deployed in a force.
    pub fn take_force_units(&mut self) -> Units {
        let ret = self.force_units();
        for (unit, count) in ret.iter() {
            debug_assert!(unit.is_mobile(Some(self.tower_type)));

            let subtracted = self.units.subtract(unit, count);
            debug_assert_eq!(subtracted, count);
        }
        ret
    }

    /// Returns the amount of mobile units diminished.
    pub(crate) fn diminish_units_if_dead_or_overflow(&mut self) -> usize {
        let mut units = 0;
        for unit in Unit::iter() {
            if self.player_id.is_none()
                || self.units.available(unit) > self.units.capacity(unit, Some(self.tower_type))
            {
                let subtracted = self.units.subtract(unit, 1);
                if unit.is_mobile(Some(self.tower_type)) {
                    units += subtracted;
                }
            }
        }
        units
    }

    pub fn unit_generation(&self, unit: Unit) -> Option<Ticks> {
        if unit != Unit::Shield && self.units.has_ruler() {
            // TODO maybe check capacity instead.
            None
        } else {
            self.tower_type.unit_generation(unit)
        }
    }

    pub fn generates_mobile_units(&self) -> bool {
        for unit in Unit::iter() {
            // Includes Projector.
            if !unit.is_mobile(Some(self.tower_type)) {
                continue;
            }
            if self.unit_generation(unit).is_some() {
                return true;
            }
        }
        false
    }

    pub fn reconcile_units(&mut self) {
        self.units
            .reconcile(self.tower_type, self.player_id.is_some());
    }

    /// Handles assertions and clearing supply line.
    /// Must call instead of mutating player_id.
    pub fn set_player_id(&mut self, player_id: Option<PlayerId>) {
        Self::set_player_id_inner(
            &mut self.player_id,
            &self.units,
            &mut self.supply_line,
            player_id,
        )
    }

    /// Inlined version of [`Self::set_player_id`].
    pub fn set_player_id_inner(
        current: &mut Option<PlayerId>,
        units: &Units,
        supply: &mut Option<Path>,
        next: Option<PlayerId>,
    ) {
        debug_assert_ne!(*current, next);
        match (*current, next) {
            (None, Some(_)) => {
                debug_assert!(supply.is_none());
                debug_assert!(!units.contains(Unit::Ruler));
                debug_assert!(!units.contains(Unit::Shield));
            }
            (Some(_), _) => {
                *supply = None;
                debug_assert!(!units.contains(Unit::Ruler));
                debug_assert!(!units.contains(Unit::Shield));
            }
            _ => unreachable!(),
        }
        *current = next;
    }
}

#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Debug,
    Display,
    EnumString,
    Serialize,
    Deserialize,
    Encode,
    Decode,
    EnumIter,
    IntoPrimitive,
    TryFromPrimitive,
    TowerTypeData,
)]
#[repr(u8)]
#[serde(into = "u8", try_from = "u8")]
#[tower(sensor_radius = 12)]
#[capacity(Ruler = 1)]
#[prerequisite(10)]
#[generate(Shield = 5)]
pub enum TowerType {
    #[tower(spawnable)]
    #[prerequisite(Runway, 20, Factory = 2, Radar = 1)]
    #[capacity(Fighter = 4, Bomber = 4, Soldier = 4, Tank = 3, Shield = 10)]
    #[generate(Bomber = 30)]
    Airfield,
    #[tower(spawnable)]
    #[prerequisite(Barracks, 25, Factory = 1, Mine = 1)]
    #[capacity(Soldier = 4, Tank = 5, Shield = 15)]
    #[generate(Tank = 15)]
    Armory,
    #[prerequisite(Bunker, 40, Refinery = 2, Radar = 3)]
    #[capacity(Shell = 3, Shield = 20)]
    #[generate(Shell = 15)]
    Artillery,
    #[tower(spawnable)]
    #[capacity(Soldier = 12, Tank = 2, Shield = 10)]
    #[generate(Soldier = 6)]
    Barracks,
    #[capacity(Shield = 1)]
    Buoy,
    #[prerequisite(Mine, 30, Headquarters = 1, Ews = 1)]
    #[capacity(Soldier = 6, Shield = 40)]
    Bunker,
    #[prerequisite(Headquarters, 40, Bunker = 10, Headquarters = 15, Projector = 20)]
    #[capacity(Soldier = 8, Tank = 2, Shield = 60)]
    #[generate(Shield = 3)]
    Capitol,
    #[prerequisite(Factory, 30, Mine = 3)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 15)]
    Centrifuge,
    #[tower(score_weight = 5)]
    #[prerequisite(Town, 30, Quarry = 2, Reactor = 1, Town = 3)]
    #[capacity(Fighter = 2, Soldier = 6, Tank = 2, Shield = 15)]
    City,
    #[capacity(Soldier = 4, Tank = 2, Shield = 30)]
    Cliff,
    #[prerequisite(Lighthouse, 30, Factory = 3)]
    #[capacity(Frigate = 4, Shield = 15)]
    #[generate(Frigate = 20)]
    Dock,
    #[prerequisite(Dock, 60, Quarry = 1, Refinery = 2)]
    #[capacity(Frigate = 3, Submarine = 3, Shield = 15)]
    #[generate(Submarine = 30)]
    Drydock,
    #[tower(sensor_radius = 20)]
    #[prerequisite(Radar, 30, Generator = 2)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 15)]
    Ews,
    #[tower(score_weight = 2)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 10)]
    Factory,
    #[capacity(Soldier = 4, Tank = 2, Shield = 10)]
    Generator,
    #[prerequisite(Village, 20, Radar = 1)]
    #[capacity(Soldier = 8, Tank = 2, Shield = 40)]
    Headquarters,
    #[tower(spawnable)]
    #[prerequisite(Airfield, 20, Armory = 2, Factory = 3)]
    #[capacity(Chopper = 3, Soldier = 4, Tank = 2, Shield = 15)]
    #[generate(Chopper = 30)]
    Helipad,
    #[tower(sensor_radius = 48)]
    #[prerequisite(Silo, 40, City = 25, Silo = 15, Rocket = 15)]
    #[capacity(Shield = 40)]
    #[generate(Shield = 3)]
    Icbm,
    #[tower(score_weight = 2)]
    #[prerequisite(Rig, 60, Reactor = 1, Radar = 1, Drydock = 1)]
    #[capacity(Shield = 30)]
    Lab,
    #[tower(sensor_radius = 48)]
    #[prerequisite(Reactor, 40, City = 25, Reactor = 15, Satellite = 15)]
    #[capacity(Shield = 40)]
    #[generate(Shield = 3)]
    Laser,
    #[prerequisite(Radar, 30, Runway = 3)]
    #[capacity(Emp = 1, Shield = 15)]
    #[generate(Emp = 80)]
    Launcher,
    #[tower(sensor_radius = 8)]
    #[capacity(Frigate = 1, Shield = 10)]
    Lighthouse,
    #[tower(score_weight = 12)]
    #[prerequisite(City, 40, City = 10, Town = 15, Village = 20)]
    #[capacity(Fighter = 2, Soldier = 6, Tank = 2, Shield = 20)]
    Metropolis,
    #[tower(score_weight = 2)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 15)]
    Mine,
    #[prerequisite(Buoy, 15, Factory = 1)]
    #[capacity(Shield = 20)]
    #[generate(Shield = 3)]
    Minefield,
    #[prerequisite(Centrifuge, 20, Rampart = 2, Reactor = 2)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 10)]
    #[generate(Shield = 3)]
    Projector,
    #[tower(score_weight = 2)]
    #[prerequisite(Cliff, 20, Village = 1)]
    #[capacity(Soldier = 6, Tank = 2, Shield = 10)]
    Quarry,
    #[tower(sensor_radius = 16)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 10)]
    Radar,
    #[prerequisite(Cliff, 20, Barracks = 2)]
    #[capacity(Soldier = 8, Shield = 45)]
    #[generate(Shield = 3)]
    Rampart,
    #[prerequisite(Generator, 40, Centrifuge = 1)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 10)]
    Reactor,
    #[tower(score_weight = 3)]
    #[prerequisite(Factory, 20, Generator = 3, Cliff = 1)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 5)]
    Refinery,
    #[tower(score_weight = 3)]
    #[prerequisite(Buoy, 30, Refinery = 1, Dock = 2)]
    #[capacity(Chopper = 2, Frigate = 1, Shield = 10)]
    Rig,
    #[prerequisite(Launcher, 20, Refinery = 1)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 15)]
    Rocket,
    #[tower(spawnable)]
    #[capacity(Fighter = 4, Soldier = 4, Tank = 2, Shield = 5)]
    #[generate(Fighter = 30)]
    Runway,
    #[tower(sensor_radius = 30)]
    #[prerequisite(Ews, 40, Rocket = 2, Generator = 5)]
    #[capacity(Soldier = 4, Tank = 2, Shield = 15)]
    Satellite,
    #[prerequisite(Quarry, 40, Centrifuge = 2, Rocket = 1)]
    #[capacity(Nuke = 1, Soldier = 4, Tank = 1, Shield = 20)]
    #[generate(Nuke = 120)]
    Silo,
    #[tower(score_weight = 2)]
    #[prerequisite(Village, 20, Generator = 1, Village = 3)]
    #[capacity(Fighter = 1, Soldier = 4, Tank = 1, Shield = 10)]
    Town,
    #[capacity(Soldier = 4, Shield = 5)]
    Village,
}

pub type TowerArray<V> = EnumArray<TowerType, V, { std::mem::variant_count::<TowerType>() }>;

impl TowerType {
    pub fn is_large(self) -> bool {
        //false
        matches!(
            self,
            Self::Capitol | Self::Icbm | Self::Laser | Self::Metropolis
        )
    }

    pub fn is_aquatic(self) -> bool {
        matches!(
            self,
            Self::Lighthouse | Self::Buoy | Self::Rig | Self::Dock | Self::Drydock
        )
    }

    pub fn scale(self) -> u8 {
        self.is_large() as u8 + 1
    }

    pub fn can_upgrade_to(self, other: Self) -> bool {
        other.downgrade() == Some(self)
    }

    pub fn upgrades(self) -> impl Iterator<Item = Self> + 'static {
        Self::iter().filter(move |&other| self.can_upgrade_to(other))
    }

    pub fn prerequisites(self) -> impl Iterator<Item = (Self, u8)> {
        TowerType::iter().filter_map(move |tower_type| {
            NonZeroU8::new(self.prerequisite(tower_type)).map(|u| (tower_type, u.get()))
        })
    }

    /// If possible user [`Tower::generates_mobile_units`] which considers if the tower is occupied
    /// by a king.
    pub fn generates_mobile_units(&self) -> bool {
        for unit in Unit::iter() {
            // Note: Includes shield projector.
            if !unit.is_mobile(Some(*self)) {
                continue;
            }
            if self.unit_generation(unit).is_some() {
                return true;
            }
        }
        false
    }

    /// Returns the max edge distance of its generated unit.
    pub fn ranged_distance(self) -> Option<u32> {
        Unit::iter()
            .filter_map(|u| self.unit_generation(u).and(u.ranged_distance()))
            .next()
    }

    /// Returns the damage a tower of this type takes from a ranged attack.
    pub fn ranged_damage(self, damage: u8) -> u8 {
        use TowerType::*;
        // Division by 3 should optimize to mul + shr
        match self {
            Bunker | Capitol => damage / 3,
            Headquarters| Icbm | Laser => damage * 2 / 3,
            Lab => damage / 2,
            _ => damage,
        }
    }

    /// Returns how much damage a tower of this type takes from an infinite damage ranged attack.
    pub fn max_ranged_damage(self) -> u8 {
        self.ranged_damage(Unit::INFINITE_DAMAGE)
    }

    /// Returns zero-indexed level. Invariant: Every tower has a higher level than its downgrade
    /// and its prerequisites.
    pub fn level(self) -> usize {
        self.prerequisites()
            .map(|(p, _)| p)
            .chain(self.downgrade())
            .map(|prerequisite| prerequisite.level())
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// Which lowest level tower can upgrade to this tower?
    pub fn basis(mut self) -> Self {
        while let Some(downgrade) = self.downgrade() {
            self = downgrade;
        }
        return self;
    }

    pub fn has_prerequisites(self, tower_counts: &TowerArray<u8>) -> bool {
        tower_counts
            .iter()
            .all(|(tower_type, &count)| count >= self.prerequisite(tower_type))
    }

    pub fn max_range() -> u16 {
        Self::iter()
            .map(Self::sensor_radius)
            .max()
            .unwrap()
            .div_ceil(TowerId::CONVERSION)
    }

    pub fn iter() -> impl Iterator<Item = Self> + Clone + 'static {
        <Self as IntoEnumIterator>::iter()
    }

    pub(crate) fn generate(hash: u8, noise_value: f64) -> Self {
        const AQUATIC_THRESHOLD: f64 = -0.25;
        let is_aquatic = noise_value < AQUATIC_THRESHOLD;
    
        let filtered_items: Vec<_> = TowerType::iter()
            .filter(|t| t.downgrade().is_none())
            .filter(|t| t.is_aquatic() == is_aquatic)
            .collect();
    
        let count = filtered_items.len();
        if count == 0 {
            panic!("Filtered iterator is empty");
        }
    
        let index = hash as usize % count;
        filtered_items[index].clone()
    }    
}

#[cfg(test)]
mod tests {
    use crate::tower::{fast_integer_sqrt, integer_sqrt, Tower, TowerId, TowerType};
    use crate::unit::Unit;
    use rand::{thread_rng, Rng};
    use test::{black_box, Bencher};

    #[test]
    fn size_of() {
        size_of!(Tower)
    }

    #[test]
    fn serialized_size() {
        serialized_size_enum!(TowerType);
        serialized_size_value!("Tower", Tower::with_type(TowerType::City));
    }

    #[test]
    fn distance() {
        assert_eq!(TowerId::new(5, 10).distance(TowerId::new(15, 10)), 49);
        assert_eq!(TowerId::new(5, 10).distance(TowerId::new(15, 11)), 49);
    }

    #[test]
    fn tower_type_max_edge_distance() {
        assert_eq!(
            TowerType::Barracks.ranged_distance(),
            Unit::Soldier.ranged_distance()
        );
        assert_eq!(
            TowerType::Artillery.ranged_distance(),
            Unit::Shell.ranged_distance()
        );
        assert_eq!(
            TowerType::Silo.ranged_distance(),
            Unit::Nuke.ranged_distance()
        );
        assert_eq!(
            TowerType::Town.ranged_distance(),
            Unit::Soldier.ranged_distance()
        );
    }

    #[test]
    fn test_integer_sqrt() {
        assert_eq!(integer_sqrt(u64::MAX), u32::MAX);

        fn test_isqrt(i: u64) {
            assert_eq!(integer_sqrt(i), (i as f32).sqrt() as u32);
            assert_eq!(fast_integer_sqrt(i), (i as f32).sqrt() as u32)
        }

        for i in 0..100000 {
            test_isqrt(i);
        }

        for i in u32::MAX as u64 - 10000..u32::MAX as u64 - 1000 {
            test_isqrt(i);
        }
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic)]
    fn test_fast_integer_sqrt() {
        fast_integer_sqrt(u64::MAX);
    }

    #[bench]
    fn bench_tower_id_offset_100(b: &mut Bencher) {
        b.iter(|| {
            for _ in 0..100 {
                black_box(black_box(&TowerId::new(123, 456)).offset());
            }
        })
    }

    fn sqrt_test_data() -> [u32; 64] {
        [(); 64].map(|_| thread_rng().gen())
    }

    #[bench]
    fn bench_integer_sqrt(b: &mut Bencher) {
        let data = sqrt_test_data();
        b.iter(|| {
            for i in data {
                black_box(integer_sqrt(black_box(i as u64)));
            }
        })
    }

    #[bench]
    fn bench_fast_integer_sqrt(b: &mut Bencher) {
        let data = sqrt_test_data();
        b.iter(|| {
            for i in data {
                black_box(fast_integer_sqrt(black_box(i as u64)));
            }
        })
    }
}
