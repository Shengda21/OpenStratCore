# 裁决流水线综合（combat-model）

这是把附录各表"接起来"的说明。各表本身见 `config/tables/`，权威真值即仓库内 `config/tables/*.json`（经双人交叉核验）。
所有随机数来自 `openstratcore_core::rng::Rng`（硬规则 #1：确定性）。

## 通用前置：通视 + 观察
1. **通视**（三.6）：六角格中心连线，线性插值高程；连线经过的居民地/丛林地 +1；高看低时观察者高程 +1。被更高高程阻挡 → 不通视。相邻格恒通视。
2. **观察**（三.7 / `observation_distance.json`）：按"观察方类型 × 被观察方类型"查可观察格数。掩蔽减半；目标在通视的居民/丛林内减半；车辆高程低于观察者时其掩蔽对观察无效。
3. 直瞄/引导/校射的"能否打"先过通视+观察+射程。

## 流水线 A：直瞄对**车辆**（2d6）
输入：武器、距离、射击方车数、射击方状态、目标地形/状态/装甲。
1. **基础攻击等级** ← `attack_level_vs_vehicle.json[武器][距离]`（步兵轻武器走 `attack_level_infantry_vs_vehicle.json`，按班数）。
2. **高度差修正** ← `height_diff_correction.json[高度差][距离]`（高看低取正、低打高取负；⚠️待复核）。得"修正后攻击等级"。
3. **段1** ← `result_vs_vehicle.json.stage1[射击方车数][攻击等级]`（车越多等级越高；⚠️待录入）。
4. 掷 **2d6** = 随机数；按 `vehicle_loss_correction.json.additive_random`（射击方/目标地形/目标状态）对随机数累加 → "修正后随机数"。
5. **段2 基础毁伤** ← `result_vs_vehicle.json.stage2[随机数][攻击等级]`（⚠️待录入）。
6. **装甲修正** ← `vehicle_loss_correction.json.by_modified_random[修正后随机数桶][目标装甲]`。
7. 合成 → 最终毁伤车数（封顶≈5）；空=无效/压制。`8.8`：损车数=对应损班数；压制 150s。

## 流水线 B：直瞄对**人员** / 步兵轻武器对车辆（结果表用 1d6 修正）
1. **攻击等级** ← `attack_level_vs_personnel.json[武器][班数][距离]`。
2. 查 `result_vs_personnel.json[随机数][攻击等级]` → `压`(压制) / 数字(毁伤班) / 空(无效)（⚠️中段待复核）。
3. **结果修正**：掷 1d6，按 `personnel_loss_correction.json.additive_random` 累加 → 查 `by_modified_random`（≤0:-1 / 1-7:0 / ≥8:+1）。
4. `8.8`：被压制步兵再被裁决压制则 −1 班，余班保持压制。

## 流水线 C：间瞄（炮兵）
计划阶段（三.9 / `timings.json`）：Fly 150s → 裁决 → Boom 300s（期间进入该格者受一次裁决）；CD 300s。可同时存在 1 爆炸 + 1 飞行点。
1. **校射等级**：无校射 / 格内校射 / 目标校射（看本方能否观察到目标格/格内目标；直升机、巡飞弹不能校射）。
2. **散布** ← `indirect_scatter.json.{no_spotting|in_hex_spotting|target_spotting_single}[随机数][距离桶]` → `命中` / `散布` / `散布n格`。
3. **战斗结果** ← 命中用 `indirect_result.json.hit`，偏离用 `.miss`，按 `[随机数][炮型]` → 数字/压制/无效。
4. **结果修正**（三.附2.6 / `indirect_correction.json`，1d6）：地形/目标状态/装甲/火炮数量累加随机数 → ≤1:-2 / 2-4:-1 / 5-7:0 / ≥8:+1。
5. 间瞄对敌我双方都生效；炮兵在 300s 内机动则取消裁决点。

## 流水线 D：防空（对空）
1. **攻击等级** ← `aa_attack_level.json[武器][距离]`（高炮20格、便携/导弹小队20格、导弹车50格；⚠️车载尾值待核）。
2. **歼灭判定** ← `aa_result.json.kill_threshold_by_random[随机数]`：攻击等级 ≥ 阈值 ⇒ 歼灭，否则无效（绕 7 对称）。
3. **多次裁决**（三.18）：一次射击产生的裁决次数 = 算子当前车/班数；射速与上限见 `timings.json.fire_intervals`。

## 流水线 E：雷场
1. **损伤** ← `minefield.json.damage_by_random[单位类型][随机数]`。
2. **装甲修正** ← `minefield.json.armor_correction_by_random[装甲][随机数]`。
3. 沿己方已开辟通路（人员）或半速沿通路（车辆）通过 → 不裁决。

## 同格交战（三.15，特殊直瞄）
距离按 0 处理，自动选最高攻击等级武器；首入瞬间立即一次（不受冷却/机动/压制限制），此后每 25s 一次；目标优先级 `坦克>战车>其他车>人员`，同类优先打班少者。脱离需机动，但机动会吃同格内所有敌方一次惩罚打击。
