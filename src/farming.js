/**
 * Farming-tier comparison — "is the harder difficulty worth it?"
 *
 * Pure and deterministic, like engine.js: takes numbers, returns numbers. All
 * I/O (reading a capture, pulling numbers out of the wire format) lives in
 * hs-sniff; callers feed `summarizeFarmingSamples` decoded account packets.
 *
 * The model: a tier swap is worth it iff reward-per-hour goes up.
 *
 *     rewardRatio  = (reward per kill on the harder tier) / (reward per kill on the easier tier)
 *     timeRatio    = (seconds per equivalent run on the harder tier) / (on the easier tier)
 *     swap iff rewardRatio > timeRatio
 *
 * `rewardRatio` is the whole argument, so it is deliberately its own function.
 * Season 10 difficulty experience bonuses come from Patch 7.0.3 (Steam).
 */

export const DIFFICULTY_XP_BONUS = Object.freeze({
  normal: 0,
  nightmare: 0.1,
  hell: 0.15,
  inferno: 0.2,
});

/**
 * Reward-per-kill ratio of `to` over `from` (e.g. inferno over hell).
 *
 * "Increased experience gain" is a single additive stat bucket, so Blood Pact
 * sliders and item affixes dilute the difficulty delta rather than scaling it:
 * +250% added to both sides pushes the ratio toward 1. A bonus that multiplies
 * the final result instead cancels out of the ratio entirely, so there is
 * nothing to pass for it — that is what `additivePool: 0` is for.
 *
 * @param {string} from easier difficulty id
 * @param {string} to harder difficulty id
 * @param {number} [baseRewardRatio] base per-kill reward of `to` over `from`
 *   before difficulty bonuses (monster level/tier difference; 1 = identical)
 * @param {number} [additivePool] additive experience-gain bonus shared by both
 *   tiers, as a fraction (2.5 = +250% Blood Pact slider at max)
 * @returns {number} multiplier, or Infinity when the reward is gated to `to`
 */
export function rewardRatio({ from, to, baseRewardRatio = 1, additivePool = 0 } = {}) {
  const fromBonus = DIFFICULTY_XP_BONUS[from] ?? 0;
  const toBonus = DIFFICULTY_XP_BONUS[to] ?? 0;
  return (baseRewardRatio * (1 + additivePool + toBonus)) / (1 + additivePool + fromBonus);
}

/**
 * How much slower the harder tier may be before it stops paying.
 * Equal to `rewardRatio` by construction; named for the decision it feeds.
 */
export function breakEvenTimeRatio(rewardRatio) {
  return rewardRatio;
}

/**
 * The decision. Strictly greater: at exact parity the easier tier wins, since
 * it has the lower variance and the lower failure cost.
 *
 * Pass rewardRatio = Infinity for a target that only exists on the harder
 * tier (high runes and runes above id 32 are Inferno-only in Season 10), which
 * makes the answer yes for any finite clear time.
 */
export function shouldFarmHarder({ rewardRatio, clearTimeRatio }) {
  const swap = rewardRatio > clearTimeRatio;
  return {
    tier: swap ? 'harder' : 'easier',
    swap,
    rewardRatio,
    clearTimeRatio,
    /** Fraction slower the harder tier could be and still break even. */
    headroom: Number.isFinite(rewardRatio) ? rewardRatio - clearTimeRatio : Infinity,
  };
}

/**
 * Fold decoded account packets into per-tier farming rates.
 *
 * Hero Siege reports cumulative experience/kills, so each sample yields a
 * delta against its predecessor. Samples must already be in wire order. Runs
 * that span a hero level-up or a table are still correct for xp-per-kill; only
 * `durationMs` (session length) is needed for per-hour figures, since the
 * packet format carries no timestamp we can trust.
 *
 * @param {Array<{experience:number,kills:number,difficulty:number|string,
 *   blood_pact?:number,hell_sub?:number}>} samples
 * @param {{durationMs?:number|null}} [options]
 * @returns {Object<string,{samples:number,kills:number,xp:number,
 *   xpPerKill:number,killsPerHour:number|null,xpPerHour:number|null}>}
 */
export function summarizeFarmingSamples(samples, { durationMs = null } = {}) {
  const buckets = new Map();

  const bucketFor = (sample) => {
    const key = `${sample.difficulty ?? 'unknown'}${Number(sample.blood_pact) ? '+bp' : ''}`;
    if (!buckets.has(key)) buckets.set(key, { samples: 0, kills: 0, xp: 0 });
    return buckets.get(key);
  };

  for (let i = 0; i < samples.length; i += 1) {
    const sample = samples[i];
    const bucket = bucketFor(sample);
    bucket.samples += 1;

    const previous = samples[i - 1];
    if (!previous || bucketFor(previous) !== bucket) continue;

    const kills = Number(sample.kills) - Number(previous.kills);
    const xp = Number(sample.experience) - Number(previous.experience);
    // Counters are cumulative; a decrease means a reset or a different
    // character, not negative farming.
    if (kills <= 0 || xp < 0) continue;

    bucket.kills += kills;
    bucket.xp += xp;
  }

  const result = {};
  for (const [key, bucket] of buckets) {
    const hours = durationMs ? durationMs / 3_600_000 : null;
    result[key] = {
      ...bucket,
      xpPerKill: bucket.kills > 0 ? bucket.xp / bucket.kills : 0,
      killsPerHour: hours ? bucket.kills / hours : null,
      xpPerHour: hours ? bucket.xp / hours : null,
    };
  }
  return result;
}
