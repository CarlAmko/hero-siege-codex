#!/usr/bin/env node
// Numeric outlier scan over hs-plan/data — "what makes a build good".
//
// The engine (src/calc/skills/damage.rs) multiplies damage in discrete buckets:
//
//   hit = (base + flat) × (1 + synergy/100) × (1 + skill_damage/100)
//         × more × extra × damage_taken × elem_break × element_break × resistance
//   dps = hit × crit.avg × multicast × projectiles × entity_count × hits_per_cast × cast_rate
//
// Gear and the passive tree feed the middle factors roughly uniformly, so what
// separates one skill from another in the data is the coefficient (`base`), how
// many times a cast lands that coefficient (`hits_per_cast` from the hit model),
// and the multipliers a skill's own subtree can buy (projectiles, share of total
// damage, summons, multicast) on top of its attribute synergy.
//
// Usage:
//   node tools/outliers.mjs [--top=N] [--class=<id>] [--metric=<name>] [--json]
//
// Metrics: power (intrinsic damage per cast, default), ceiling (power after a
//          400-point attribute synergy and a greedy 20-point subskill budget),
//          base20, coefficient, d1, growth, weapon20, hits, proj, ofTotal,
//          summon, multicast, attrSyn, synergyIn, synergyOut, subDamageCeil,
//          buffStats, and the unbounded ceil* sums of each subtree effect.

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

const DATA = join(dirname(fileURLToPath(import.meta.url)), "..", "data");
const read = (rel) => JSON.parse(readFileSync(join(DATA, rel), "utf8"));

const RANK = 20; // every skill in the corpus caps at 20 ranks

// ---------- load ----------

const skills = [];
for (const file of readdirSync(join(DATA, "skills")).sort()) {
  if (!file.endsWith(".json")) continue;
  const classId = basename(file, ".json");
  for (const skill of read(join("skills", file))) skills.push({ ...skill, classId });
}
const classes = Object.fromEntries(
  readdirSync(join(DATA, "classes"))
    .filter((f) => f.endsWith(".json"))
    .map((f) => [basename(f, ".json"), read(join("classes", f))]),
);
const gameConfig = read("game-config.json");
const attrPoints = gameConfig.attributePointsPerLevel * gameConfig.maxCharacterLevel;
const subskillPoints = Math.floor(gameConfig.maxCharacterLevel / gameConfig.levelsPerSubskillPoint);

// ---------- per-skill metrics ----------

const sumSubskills = (skill, pick) => {
  let total = 0;
  for (const node of skill.subskills ?? []) {
    const effect = node.effects ?? {};
    total += pick(effect.base ?? {});
    total += pick(effect.perRank ?? {}) * (node.maxRank ?? 0);
  }
  return total;
};

// A subskill node at rank r contributes `base + perRank × r`
// (src/calc/subskill.rs:apply_effect). Points are scarce — one per
// `levelsPerSubskillPoint` levels — so the *reachable* tree bonus is a greedy
// allocation over the damage multipliers, not the sum of every node's max.
const MEASURED = ["projectile_count", "of_total_damage", "summon_max_amount",
  "max_summoned_guardians", "multicast_chance"];

const subtreeValue = (state) =>
  (1 + state.projectile_count) *
  (1 + (state.of_total_damage ?? 0) / 100) *
  (1 + (state.summon_max_amount ?? 0) + (state.max_summoned_guardians ?? 0)) *
  (1 + (state.multicast_chance ?? 0) / 100);

// Greedy: repeatedly buy the single subskill rank that multiplies the tree's
// damage the most, until the point budget runs out.
const budgetSubtree = (skill, points) => {
  const state = Object.fromEntries(MEASURED.map((k) => [k, 0]));
  const damageKey = skill.damageType ? `${skill.damageType}_skill_damage` : "";
  let damage = 0;
  const nodes = (skill.subskills ?? [])
    .filter((n) => (n.maxRank ?? 0) > 0)
    .map((n) => ({
      base: n.effects?.base ?? {},
      perRank: n.effects?.perRank ?? {},
      left: n.maxRank,
      taken: 0,
    }))
    .filter((n) =>
      [...MEASURED, damageKey].some((k) => k && (n.perRank[k] || n.base[k])),
    );
  // A rank adds base once (on the first point) and perRank every point.
  const apply = (n) => {
    const first = n.taken === 0;
    for (const k of MEASURED) state[k] += (n.perRank[k] ?? 0) + (first ? n.base[k] ?? 0 : 0);
    damage += (n.perRank[damageKey] ?? 0) + (first ? n.base[damageKey] ?? 0 : 0);
  };
  const value = () => value0(state, damage);
  for (let p = 0; p < points; p++) {
    let best = null;
    let bestGain = 1;
    for (const n of nodes) {
      if (n.left <= 0) continue;
      const snapshot = { state: { ...state }, damage };
      apply(n);
      const gain = value() / value0(snapshot.state, snapshot.damage);
      Object.assign(state, snapshot.state);
      damage = snapshot.damage;
      if (gain > bestGain) {
        bestGain = gain;
        best = n;
      }
    }
    if (!best) break;
    apply(best);
    best.left--;
    best.taken++;
  }
  return { state, damage };
};
const value0 = (state, damage) => subtreeValue(state) * (1 + damage / 100);

// Damage at `rank` ranks allocated: base + perLevel × rank, matching
// src/calc/skills/damage.rs (the linear formula includes the allocated rank).
const damageAt = (s, rank) => {
  if (s.damageFormula) return s.damageFormula.base + s.damageFormula.perLevel * rank;
  const table = s.damagePerRank;
  if (table?.length) {
    const row = table[Math.min(rank, table.length) - 1];
    return row.max ?? row.min ?? null;
  }
  return null;
};

// Mirrors `hits_per_cast` in src/calc/build.rs: one hit, plus one per full tick
// of the damage object's lifetime.
const hitsPerCast = (s) => {
  const hm = s.hitModel;
  if (!hm?.tickFrequency || !hm?.lifetime) return 1;
  return Math.floor(hm.lifetime / hm.tickFrequency) + 1;
};

const bonusOf = (bs) => (bs.per === "attribute_point" ? "attr" : bs.per === "skill_level" ? "skill" : null);

const metrics = (s) => {
  const sc = s.attackScaling ?? {};
  const d1 = damageAt(s, 1);
  const d20 = damageAt(s, RANK);
  const w1 = sc.weaponDamagePct ? sc.weaponDamagePct.base + sc.weaponDamagePct.perLevel : null;
  const w20 = sc.weaponDamagePct ? sc.weaponDamagePct.base + sc.weaponDamagePct.perLevel * RANK : null;
  const fp20 = sc.flatPhysicalMax ? sc.flatPhysicalMax.base + sc.flatPhysicalMax.perLevel * RANK : null;
  const hits = hitsPerCast(s);
  // Attack skills (`attackKind: attack`) run through the weapon path: their
  // coefficient is a share of weapon damage, they gain no `magic_skill_damage`
  // bucket, and only their `damageFormula` elemental share reaches the spell
  // buckets. Spells are the inverse.
  const isAttack = s.attackKind === "attack";
  const archetype = isAttack ? "attack" : d20 != null ? "spell" : "none";
  const ceil = (pick) => sumSubskills(s, pick);
  const ceilProj = ceil((m) => m.projectile_count ?? 0);
  const ceilOfTotal = ceil((m) => m.of_total_damage ?? 0);
  const ceilSummon = ceil((m) => (m.summon_max_amount ?? 0) + (m.max_summoned_guardians ?? 0));
  const ceilMulticast = ceil((m) => m.multicast_chance ?? 0);
  const subDamageCeil = s.damageType
    ? ceil((m) => m[`${s.damageType}_skill_damage`] ?? 0)
    : 0;

  const { state: tree, damage: treeDamage } = budgetSubtree(s, subskillPoints, 0);
  const proj = 1 + (tree.projectile_count ?? 0);
  const ofTotal = tree.of_total_damage ?? 0;
  const summon = 1 + (tree.summon_max_amount ?? 0) + (tree.max_summoned_guardians ?? 0);
  const multicast = 1 + (tree.multicast_chance ?? 0) / 100;

  // Attribute synergy: % skill damage per point, ×400 points at level cap.
  let attrSyn = 0;
  let synergyOut = 0;
  for (const bs of s.bonusSources ?? []) {
    if (bonusOf(bs) === "attr") attrSyn += bs.value;
    if (bonusOf(bs) === "skill") synergyOut += bs.value;
  }
  // Skill-level synergy: other skills that raise this one's rank.
  let synergyIn = 0;
  let synergyInCount = 0;
  const name = s.name.trim().toLowerCase();
  for (const other of skills) {
    for (const bs of other.bonusSources ?? []) {
      if (bonusOf(bs) !== "skill") continue;
      if (bs.source.trim().toLowerCase() === name) {
        synergyIn += bs.value;
        synergyInCount++;
      }
    }
  }

  // Coefficient the engine feeds into the `base` bucket: spell damage for cast
  // skills, weapon-% for attack skills (those need a weapon to land).
  const coefficient = d20 ?? w20 ?? 0;
  // `power`: intrinsic damage per cast — the coefficient the engine multiplies
  // by `hits_per_cast`. No gear, no tree, no attributes.
  const power = coefficient * hits;
  // `ceiling`: the same skill after a full 400-point attribute synergy and the
  // greedy 20-point subskill budget. A heuristic upper bound, not a build.
  const ceiling =
    power *
    proj *
    (1 + ofTotal / 100) *
    summon *
    multicast *
    (1 + (attrSyn * attrPoints) / 100) *
    (1 + treeDamage / 100);

  let buffStats = Object.values(s.passiveStats?.base ?? {}).reduce((n, v) => n + v, 0);
  for (const v of Object.values(s.passiveStats?.perRank ?? {})) buffStats += v * RANK;

  return {
    archetype,
    // Only a `damageFormula` share of an attack reaches the spell buckets;
    // 56 of 85 attacks have none and are pure weapon damage.
    elementalPart: Boolean(s.damageFormula),
    base20: d20,
    weapon20: w20,
    flatPhysical20: fp20,
    coefficient,
    d1: d1 ?? w1,
    growth: d1 && d20 ? d20 / d1 : null,
    hits,
    proj,
    ofTotal,
    summon,
    multicast,
    ceilProj: 1 + ceilProj,
    ceilOfTotal,
    ceilSummon: 1 + ceilSummon,
    ceilMulticast: 1 + ceilMulticast / 100,
    subDamageByBudget: treeDamage,
    attrSyn,
    synergyIn,
    synergyInCount,
    synergyOut,
    subDamageCeil,
    buffStats,
    cooldown: s.baseCooldown ?? null,
    // The engine divides by cooldown only on the usesSkillHaste path
    // (src/calc/build.rs:465-489); every other cooldown skill is modelled at
    // one cast per second.
    cooldownBlind: Boolean(s.baseCooldown) && !s.usesSkillHaste && !s.usesAttackSpeed,
    power,
    ceiling,
  };
};

const sorted = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
const rows = skills.map((s) => ({
  id: s.id,
  name: s.name,
  classId: s.classId,
  className: classes[s.classId]?.name ?? s.classId,
  kind: s.kind,
  element: s.damageType ?? null,
  tags: s.tags ?? [],
  requiresLevel: s.requiresLevel ?? null,
  tree: s.tree ?? null,
  ...metrics(s),
}));

// ---------- robust outliers ----------

const positive = (values) => values.filter((v) => typeof v === "number" && v > 0);
const median = (xs) => {
  const s = [...xs].sort(sorted);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
// Robust center/spread of a positive metric in log space. Metrics that are
// mostly identical (e.g. hits per cast: 1 for everything but a handful of tick
// fields) have a zero MAD, so fall back to the log spread.
const logStats = (xs) => {
  const logs = xs.map(Math.log10);
  const center = median(logs);
  const mad = 1.4826 * median(logs.map((v) => Math.abs(v - center)));
  if (mad > 0) return { center, scale: mad, median: 10 ** center };
  const variance = logs.reduce((n, v) => n + (v - center) ** 2, 0) / logs.length;
  return { center, scale: Math.sqrt(variance) || 1e-9, median: 10 ** center };
};

const METRICS = ["power", "ceiling", "base20", "coefficient", "d1", "growth", "weapon20", "hits",
  "proj", "ofTotal", "summon", "multicast", "attrSyn", "synergyIn", "synergyOut",
  "subDamageCeil", "buffStats", "ceilProj", "ceilOfTotal", "ceilSummon", "ceilMulticast"];

const stats = new Map();
for (const key of METRICS) {
  const xs = positive(rows.map((r) => r[key]));
  stats.set(key, { n: xs.length, ...logStats(xs) });
}
for (const r of rows) {
  for (const key of METRICS) {
    const s = stats.get(key);
    r[`z_${key}`] = r[key] > 0 && s.n ? (Math.log10(r[key]) - s.center) / s.scale : 0;
  }
  r.z = r.z_power;
}

// ---------- CLI ----------

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const hit = args.find((a) => a.startsWith(`--${name}=`));
  return hit ? hit.slice(name.length + 3) : fallback;
};
const top = Number(flag("top", 25));
const classFilter = flag("class", null);
const archetypeFilter = flag("archetype", null);
const metric = flag("metric", "power");
const asJson = args.includes("--json");

if (!METRICS.includes(metric)) {
  console.error(`unknown --metric=${metric}; expected one of ${METRICS.join(", ")}`);
  process.exit(2);
}
if (archetypeFilter && !["attack", "spell"].includes(archetypeFilter)) {
  console.error("--archetype must be attack or spell");
  process.exit(2);
}

const scoped = rows
  .filter((r) => r.coefficient > 0 || r.hits > 1 || r.summon > 1 || r.proj > 1)
  .filter((r) => !classFilter || r.classId === classFilter)
  .filter((r) => !archetypeFilter || r.archetype === archetypeFilter);

const fmt = (v, digits = 0) =>
  v == null ? "-" : Math.abs(v) >= 1000 ? Math.round(v).toLocaleString("en-US") : v.toFixed(digits);
// Cooldown-blind skills report one cast per second in the engine; show the
// honest per-second rate beside it so the ranking is not read as sustained DPS.
const sustain = (r) =>
  fmt(r.cooldownBlind && r.cooldown > 1 ? r.power / r.cooldown : r.power);

if (asJson) {
  process.stdout.write(
    JSON.stringify(
      {
        corpus: {
          skills: rows.length,
          classes: Object.keys(classes).length,
          damageSkills: scoped.length,
          attributePoints: attrPoints,
          subskillPoints,
          maxCharacterLevel: gameConfig.maxCharacterLevel,
        },
        metrics: Object.fromEntries([...stats].map(([k, v]) => [k, { n: v.n, median: v.median }])),
        outliers: Object.fromEntries(
          METRICS.map((k) => [
            k,
            [...scoped]
              .sort((a, b) => b[`z_${k}`] - a[`z_${k}`])
              .slice(0, top)
              .map((r) => ({ id: r.id, class: r.classId, value: r[k], z: +r[`z_${k}`].toFixed(2) })),
          ]),
        ),
        flags: {
          archetypes: {
            attack: scoped.filter((r) => r.archetype === "attack").length,
            spell: scoped.filter((r) => r.archetype === "spell").length,
          },
          attackWithoutAttributeSynergy: scoped
            .filter((r) => r.archetype === "attack" && r.attrSyn === 0)
            .map((r) => r.id),
          cooldownBlind: scoped.filter((r) => r.cooldownBlind).map((r) => r.id),
          tickFields: scoped.filter((r) => r.hits > 1).map((r) => ({ id: r.id, hits: r.hits })),
          summoners: scoped.filter((r) => r.summon > 1).map((r) => ({ id: r.id, entities: r.summon })),
        },
        skills: scoped,
      },
      null,
      2,
    ) + "\n",
  );
} else {
  console.log(
    `corpus: ${rows.length} skills / ${Object.keys(classes).length} classes, ` +
      `${scoped.length} damage skills, level ${gameConfig.maxCharacterLevel}, ` +
      `${attrPoints} attribute points, ${subskillPoints} subskill points`,
  );
  console.log(
    `\n== top ${top} by "${metric}" (robust z on log10; median ${fmt(stats.get(metric).median)})`,
  );
  const cols = metric === "power" || metric === "ceiling"
    ? ["power", "ceiling"]
    : [metric, "power", "ceiling"];
  console.log(
    ["     z", ...cols.map((c) => c.padStart(12)), "arch".padEnd(6), "hits", "proj", "summon",
      "ofTot%", "attrSyn/pt", "     dps~", "skill"].join(" "),
  );
  for (const r of [...scoped].sort((a, b) => b[`z_${metric}`] - a[`z_${metric}`]).slice(0, top)) {
    console.log(
      [
        fmt(r[`z_${metric}`], 1).padStart(6),
        ...cols.map((c) => fmt(r[c]).padStart(12)),
        r.archetype.padEnd(6),
        String(r.hits).padStart(4),
        String(r.proj).padStart(4),
        String(r.summon).padStart(6),
        fmt(r.ofTotal).padStart(6),
        r.attrSyn.toFixed(2).padStart(9),
        sustain(r).padStart(9),
        `${r.classId}/${r.id}`,
      ].join(" "),
    );
  }
  console.log(
    "  note: attack `power` is % of weapon damage per cast, spell `power` is flat damage per cast — " +
      "not comparable without a weapon.",
  );

  const flagged = scoped.filter((r) => r.z_power >= 3.5);
  console.log(
    `\n== power outliers |z| >= 3.5: ${flagged.length} ` +
      `(${(100 * flagged.length / scoped.length).toFixed(1)}% of damage skills); ` +
      `max/median = ${fmt(Math.max(...scoped.map((r) => r.power)) / stats.get("power").median, 1)}x`,
  );

  console.log("\n== class rollup by archetype (median intrinsic power / invested ceiling, best ceiling)");
  const byClass = new Map();
  for (const r of scoped) {
    const key = `${r.classId}/${r.archetype}`;
    byClass.set(key, [...(byClass.get(key) ?? []), r]);
  }
  for (const [key, list] of [...byClass]
    .map(([id, list]) => [id, list, median(list.map((r) => r.ceiling))])
    .sort((a, b) => b[2] - a[2])) {
    const best = list.reduce((a, b) => (b.ceiling > a.ceiling ? b : a));
    console.log(
      `${key.padEnd(24)} n=${String(list.length).padStart(2)}  ` +
        `power=${fmt(median(list.map((r) => r.power))).padStart(10)}  ` +
        `ceiling=${fmt(median(list.map((r) => r.ceiling))).padStart(12)}  ` +
        `best=${fmt(best.ceiling).padStart(12)}  ${best.id}`,
    );
  }

  console.log("\n== flags");
  const atk = scoped.filter((r) => r.archetype === "attack");
  const spl = scoped.filter((r) => r.archetype === "spell");
  const atkNoLever = atk.filter((r) => r.attrSyn === 0);
  console.log(
    `archetypes: ${atk.length} attack / ${spl.length} spell. ` +
      `Attribute synergy — attack ${atk.length - atkNoLever.length}/${atk.length}, spell ` +
      `${spl.filter((r) => r.attrSyn > 0).length}/${spl.length}. ` +
      `Elemental part (damageFormula) on ${atk.filter((r) => r.elementalPart).length}/${atk.length} attacks; ` +
      `the other ${atkNoLever.filter((r) => !r.elementalPart).length} scale only from weapon damage ` +
      `(Strength: +1% enhanced damage/point, no multiplicative bucket).`,
  );
  const blind = scoped.filter((r) => r.cooldownBlind);
  console.log(
    `cooldown-blind in the engine (baseCooldown && !usesSkillHaste): ${blind.length} skills, ` +
      `cooldowns up to ${Math.max(0, ...blind.map((r) => r.cooldown ?? 0))}s — engine DPS ignores them`,
  );
  const ticks = scoped.filter((r) => r.hits > 1).sort((a, b) => b.hits - a.hits);
  console.log(
    `tick fields (hit-model ticks per cast): ${ticks.length} skills — ` +
      ticks.slice(0, 8).map((r) => `${r.id}×${r.hits}`).join(", "),
  );
  const summoners = scoped.filter((r) => r.summon > 1).sort((a, b) => b.summon - a.summon);
  console.log(
    `summon ceiling: ${summoners.slice(0, 8).map((r) => `${r.id}=${r.summon}`).join(", ")}`,
  );
  const noAttr = Object.keys(classes).filter(
    (id) => !rows.some((r) => r.classId === id && r.attrSyn > 0),
  );
  console.log(`classes with no attribute synergy: ${noAttr.join(", ") || "none"}`);
}
