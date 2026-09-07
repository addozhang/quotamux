# Scheduled Flash configuration

Official sources checked on 2026-09-07. Prices below are USD per one million
tokens in **cached input / uncached input / output** order. They are configurable
data, never constants embedded in the executable.

| Provider / model | Default | Weekday peak | Peak windows (UTC, Monday–Friday) |
| --- | --- | --- | --- |
| Ollama Flash `deepseek-v4-flash:0731` | 0.007 / 0.22 / 0.66 | 0.014 / 0.44 / 1.32 | 12:00–18:00 |
| Ollama Pro `deepseek-v4-pro:0813` | 0.022 / 0.66 / 1.98 | 0.044 / 1.32 / 3.96 | 12:00–18:00 |
| DeepSeek Flash and Flash Vision | 0.007 / 0.22 / 0.66 | 0.014 / 0.44 / 1.32 | 01:00–04:00, 06:00–10:00 |
| DeepSeek Pro | 0.022 / 0.66 / 1.98 | 0.044 / 1.32 / 3.96 | 01:00–04:00, 06:00–10:00 |
| OpenCode Go Flash and Flash Vision | 0.007 / 0.22 / 0.66 | 0.014 / 0.44 / 1.32 | 01:00–04:00, 06:00–10:00 |
| OpenCode Go Pro | 0.022 / 0.66 / 1.98 | 0.044 / 1.32 / 3.96 | 01:00–04:00, 06:00–10:00 |
| OpenCode Go GLM-5.3-Flash | 0.03 / 0.15 / 0.50 | Same | None published |

Sources: [Ollama pricing](https://ollama.com/pricing),
[DeepSeek USD pricing](https://api-docs.deepseek.com/quick_start/pricing/),
[OpenCode Go](https://opencode.ai/docs/go/).

Ollama Pro is the user's confirmed plan: three requests share one account pool.
Its listed monthly subscription is $20 with $60 of monthly usage credits.
Ollama rates value consumed credits. OpenCode Go's listed $10 monthly
subscription uses dollar-denominated allowances; these rates value allowance
consumption. DeepSeek official rates estimate pay-as-you-go usage. None of these
estimates replaces the provider's account statement. The sources above support
these billing distinctions.

GLM Coding Plan publishes points for GLM-5.3-Flash: weekday 14:00–18:00 in
UTC+8 uses `(uncached × 2.3 + cached × 0.56 + output × 8) / 10000`;
other times use half. Keep its USD pricing absent and retain that rule in
Accounts notes. [Zhipu plan documentation](https://docs.bigmodel.cn/cn/coding-plan/overview).

Kimi Code uses membership credits and quota windows, without a published exact
USD token rate for this account. Keep USD pricing absent and record membership
billing context and its official reference in Accounts.
[Kimi Code membership](https://www.kimi.com/code/docs/kimi-code/membership.html).

## Route to configure

Create `deepseek-v4-flash-scheduled` with all three supported client protocols.
Keep the existing five public routes and their layer order intact.

| Time | Layer priority |
| --- | --- |
| Default | Ollama Cloud → OpenCode Go plan → DeepSeek official PAYG |
| Monday–Friday 12:00–18:00 UTC | OpenCode Go plan → Ollama Cloud → DeepSeek official PAYG |

The Ollama peak interval is 20:00–02:00 Beijing time, belonging to its start
weekday. Each layer has one exact backend/credential/model target. The shared
`ollama-account` capacity pool is three across Flash and Pro. An occupied pool
immediately falls through to the next eligible layer; existing requests retain
their admitted order and price. This policy spends included allowance before
PAYG and changes preference while Ollama's rate is higher than Go's. It does not
query balances or invent dynamic budget rules.

## Acceptance procedure

1. On isolated 8081, configure a fresh temporary Flash route through the API;
   verify returned graph, private TOML, real protocols and route metadata.
2. Remove the temporary route. Use actual webpage controls to create the final
   named route, including all targets and its weekly rule; save and compare
   the persisted graph with this plan, then invoke real protocols again.
3. Check both sides of every price/priority boundary and weekends. Automated
   elapsed-time tests verify switching without writes or restarts; a streaming
   test crosses a price boundary and save while preserving its initial rate.
4. Validate scoped Statistics/History, All views, dark styling and read-only
   controls in the final image. Promote that image and validated document only
   after the complete test suite passes; smoke-test production on 8080.
