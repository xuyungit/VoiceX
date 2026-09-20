# LLM Bench 评分判别器：实现审计与优化方案

> 文档状态：审计结论 + 方案提议，待拍板；本次审计没有改动 `tools/llm-bench/` 下的任何文件（`src/main.rs`、`typesafe_questions.json`、`config.example.toml`）
> 被审计的对象：当前工作区里的 TypeSafe 判别器（下称"现行实现"或 v3）。它**还没有提交过**——`HEAD` 里仍是更早的自由文本 LLM 评委——所以替换它没有迁移成本，也不建议把它原样提交
> 日期：2026-09-20
> 前置：`llm-bench-typesafe-judge.md`（同日早些时候的接入记录）。该文档中"fidelity 与规则负相关，说明它提供了独立信息"的解读是错的，本文 §1.1 推翻了它
> 复现材料：`.local-notes/llm-bench-judge-v4/`（本地工作目录，不入库；见 §7）

## 0. 结论摘要

| 主题 | 结论 |
|---|---|
| 判别器为什么没起作用 | 不是模型能力不够，是用法错了。现行实现的 `quality` 分数在多错误用例上与"修对了几处"**负相关**：三处全修对的输出平均 7.84 分，一处没修的反而 8.03 分。权重最高的 `fidelity`（0.55）在惩罚正确的纠正 |
| 根因 | 三层：① `fidelity` 以 ASR 原文为锚点，改得越多"越不忠实"；② 把 `diff` 能精确算出来的事交给模型做整体打分；③ 唯一有区分度的维度 `asr_fix` 被我以"与规则重复"为由权重设成了 0 |
| 实现缺陷 | 8 处（§1.3）：相同文本重复评分且分数不一致、判别器失败被静默算成 0 分、无重试、置信度只求平均不路由、概率分布被丢弃、逐条结论不落盘、测试锁死了错误设计 |
| 我自己的失误 | 把 −0.76 的负相关读成"正交信息"；所有准确率都是样本内的；原型里有一个截取 bug，一度让留出集看起来像判别器失败（§1.4） |
| 新方案 | 差异驱动：代码算出"该改哪里"和"实际改了哪里"，代码能定的先定，定不了的才问模型，而且每次只问**一处**。三个问题：单点是否修对（3 级）、多改的地方属于什么性质（5 选 1）、整体是不是一份转写（4 选 1） |
| 证据 | 60 条真实输出：r(规则命中, correction) = +0.975，3 次独立运行 0 次结论翻转，每次 19 次调用、$0.0006（现行方案 60 次、约 $0.005）。留出集 H2（74 个变体，跑之前冻结标注）：73/74，模型判定部分 35/36 |
| 没达标的地方 | H2 预先登记的标准是"没有未被标记的严重错误"。实际有 1 个：`保险→保销`（应为 报销，只改对一半）被判"部分正确"，置信度 0.46，刚好高于 0.4 的复核线。严格说**没过线** |
| 建议 | 分期。第一期只让判别器负责规则看不到的维度（克制度 restraint：有没有乱改、乱加、答非所问），规则继续管三个档位；第二期等单点问题过了样本外标准、你审过并钉住有争议的结论之后，再让它接管档位 |

## 1. 现行实现的问题

### 1.1 方向反了

用例 "Multiple errors"（3 个检查点），30 条真实输出，按规则命中数分组
（数据：`data/ts_scores.json`，脚本：`shipped_inversion.py`，离线可复现）：

| 规则命中 | n | 现行 quality | fidelity（权重 0.55） | asr_fix（权重 0） |
|---|---|---|---|---|
| 0/3 | 2 | 8.03 | 0.980 | 0.042 |
| 1/3 | 12 | 8.00 | 0.976 | 0.590 |
| 2/3 | 10 | 8.19 | 0.948 | 0.705 |
| 3/3 | 6 | **7.84** | **0.902** | 0.795 |

| 相关系数（与规则命中数） | r |
|---|---|
| fidelity | **−0.758** |
| fluency | +0.288 |
| asr_fix | +0.774 |
| 现行的 quality | **−0.123** |

修得越多，fidelity 越低，quality 跟着降。原因在问题本身：`fidelity` 问的是"输出相对
`asr_original` 是否保留了原意"。把 `cloud md` 改成 `CLAUDE.md`、把 `下一集` 改成 `下一级`，
相对于 ASR 原文确实是"改了意思"——模型按字面回答，答得没错，是问题问反了。

前一份文档（`llm-bench-typesafe-judge.md` 第 155–159 行）写的是"fidelity 与检查点相关性
−0.778，说明它提供独立信息，值得保留"。这是我的误读：负相关不是"独立"，是"反向"。
（−0.778 是当时的数字，按现在的脚本可复现的是 −0.758。）

### 1.2 问题设计为什么必然失效

- **该让代码算的交给了模型。** 真实输出与参考答案之间只差 0–4 处。"哪里变了"是 `diff`
  能精确给出的；让模型通读两段两百字的文本再打一个整体分，等于让它自己做 diff，
  再把 3 处差异的信息压进一个 3 级分数里。TypeSafe 的文档明确建议：不要问代码能算的事，
  问题要拆小，只给问题需要的 state。
- **整体维度在这份数据上天然是平的。** 15 个 provider 的维度均值极差：fidelity 0.054、
  fluency 0.080、clean 0.047，而 asr_fix 是 0.395。输出本来就都是干净的转写，前三个维度
  没有东西可区分；quality 的差距主要来自噪声（I1）和上面那个反向的 fidelity。
- **把期望分当量值用。** 厂商建议把 `score` 的期望值和阈值比较、按置信度路由；
  现行实现直接把期望值线性映射进 10 分制。
- **`asr_fix` 权重为 0。** 我的理由是"它和规则检查点相关 r≈0.89，算进去等于重复计分"。
  但它是四个维度里唯一在区分 provider 的；把它归零以后，剩下的就是噪声加反向信号。
  正确的推论应该是：既然判别器在"修没修对"上只能复述规则，它的价值必须来自规则
  **看不到**的地方——而现行的问题并没有去问那些地方。

### 1.3 实现缺陷

行号对应当前工作区的 `tools/llm-bench/src/main.rs`。

| # | 位置 | 问题 | 后果 |
|---|---|---|---|
| I1 | 2149（`jobs.push`） | 不去重。每个 (provider, round) 都发一次请求 | 60 次调用里只有 15 段不同的文本。同一段文本的得分最多相差 0.24；如果让相同文本得相同分，15 个 provider 里有 9 个的 quality 名次会变。重复调用的噪声 sd 0.022，而 provider 之间的总极差只有 0.405（去重后 0.381） |
| I2 | 2199–2202 | `ts_unit(..).unwrap_or(0.0)` / `unwrap_or(1.0)`，键名硬编码 | 题库文件是用户可改的。改了键名或题型，缺失的维度会被静默记成 0 分（clean 记成 1），没有任何报错 |
| I3 | 2191、2245–2250 | 判别器调用失败 → 只 `failures.push`，该轮不计入 `n`；全部失败的 provider 显示 "no successful round to score" | 把"判别器没答"和"provider 没输出"混为一谈；只打印第一条失败 |
| I4 | 2081–2094 | 对 429 / 529 不重试 | 并发 8 时偶发限流直接变成 I3 |
| I5 | 2203–2209、2238 | 置信度只是求平均后打印 | 单条 0.18–0.26 的低置信回答藏在 0.67–0.80 的 provider 均值后面，没有任何一条被标出来复核；没收集到置信度时仍然除以 `n` |
| I6 | 1971（`TsAnswer`） | 只反序列化 `score` / `noul` / `confidence`，丢弃 `probabilities` 和 `choice` | 无法用期望得分；如果把题型改成 `choice`，回答会被解析成全 None，再经 I2 变成 0 分 |
| I7 | 874–888 | 只持久化 provider 级聚合 | 事后无法回答"哪一条输出、哪一个问题、为什么是这个分"。本次审计是另外重跑才拿到逐条数据的 |
| I8 | 3396 起的测试；57、2271 的文案 | `asr_fix_is_excluded_by_default_so_checkpoints_are_not_counted_twice` 只测算术，把错误的设计决定锁成了测试；没有任何"完美输出 ≥ 原样输出"的序关系测试；"r≈0.89" 写死在程序输出和注释里 | 一个最简单的序关系测试当时就能发现 §1.1 |

### 1.4 验证流程的失误

这一节是我自己的问题，单列出来，因为它们比代码缺陷更该避免。

1. **把负相关读成了优点。** 见 §1.1。没有先问"这个分数的方向对不对"，就去讨论"它独不独立"。
2. **早期的准确率全是样本内的。** 前一轮报告的 "27/29" 等数字，是在我边看结果边改问题的
   同一批数据上得到的，不能当验证。本轮改为：标注先写死、文件哈希先记录、主配置和通过标准
   先登记、跑完不再调。
3. **原型里的截取 bug，一度被我归因给判别器。** 第一个留出集 H1 的单点结果是 39/44，
   5 个严重错误，全部是"只改对一半"的中文词（`反力→返力`、`张伟→章伟` 等）。
   我最初的判断是"问题问得不够严"，并准备收紧问题。检查发给模型的 state 之后才发现：
   当 provider 只改了目标词的一部分时，原型把**那次编辑的替换文本**（`返`）当成了
   `written`，而不是该位置现在的完整内容（`返力`）。模型看到 `intended=返利, written=返`，
   回答"缺了一部分"——对它看到的东西来说是对的。修掉 bug、问题一字不改，H1 是 44/44。
   而我原本打算采用的"收紧版问题"在开发集上并不比原问题好：准确率相当，没把握的回答多一倍，
   出的错在两次运行之间还会翻转（§3.6、§4.3）。
   教训：判别器答错时，先看它**实际收到了什么**。
4. **H1 在我看过结果之后就不再是留出集了**，所以又做了 H2（§4.5）。H2 只跑了一次，之后没有再调。

## 2. 规则僵化在哪里

现在的检查点是不区分大小写的子串 / 正则。它在真实数据上并不差——这 60 条输出里，
规则在"词有没有修对"上没有判错任何一条（§4.2）。它的问题是结构性的，在构造的反例上才显形（§4.1）：

| 类型 | 例子 | 规则给分 | 应该是 |
|---|---|---|---|
| 漏判：合理的变体不在白名单里 | `下一集` 改成了 `下一层`（意思对） | 0.67 | 接近满分 |
| 误判：子串出现在任何位置都算数 | 保留 `Cloud.md`，末尾加一句"（注：Claude 相关）" | **1.00** | 没修对 + 多了废话 |
| 盲区：检查点之外的内容完全不看 | 把"不要记录细节"改成"要记录细节"；结尾编造一段；开头加"好的，以下是…"；直接输出摘要 | 全部 **1.00** | 意思被破坏 / 不是转写 |
| 编写成本 | 每个检查点要手写 `must_contain` / `must_not_contain` / 正则，并预先想全所有可接受写法 | — | 新方案下一个用例只需要 `input` + `expected` |

第三类最要命：一个把否定词改没了的 provider 和一个完美的 provider，在现有 bench 里同分。
这正是"需要一个灵活的判别器"的地方，也是第一期方案的着力点。

## 3. 新方案：差异驱动的判别

### 3.1 流程

```
required = diff(input → expected)      # 该改的地方，代码算
actual   = diff(input → output)        # 实际改的地方，代码算

对 required 里的每一处（site）：
    取同一个区间上的三段文本  heard / intended / written
    settle(heard, intended, written)   # 代码级联，见 3.2
    代码定不了 → 问 SITE_Q（3 级），只给这一处所在的句子，且句子里别的地方都已替换成正确内容

actual 里没有被任何 site 认领的编辑：
    按句子分组 → 每组问一次 EDIT_Q（5 选 1）   # before = 该句的正确版本，after = 加上这组编辑

整段输出问一次 GATE_Q（4 选 1）           # 是不是一份完整、没加料的转写

correction = 各 site 得分的平均
restraint  = 各组 EDIT 得分的乘积；gate 不是 transcript 则为 0
```

两条设计原则：

- **隔离。** 判别器每次看到的两三句话只在被判的那一处不同。它不需要自己找差异，也不会被
  别处的差异干扰。对比现行实现：两段两百字全文 + 三个整体性问题。
- **代码只定"按定义就精确"的事。** 代码的结论不会被标记复核，所以它不能靠启发式；
  模型的结论至少带置信度，错了有机会被拦下来。

### 3.2 代码级联 `settle()`

按顺序，命中即停：

| 顺序 | 条件 | 结论 | 得分 |
|---|---|---|---|
| 1 | `written == intended` | `code:exact` | 1.0 |
| 2 | `written == heard` | `code:unchanged` | 0 |
| 3 | `written.casefold() == intended.casefold()` | `code:form-case`（只差大小写） | `FORM_CREDIT["case"]` = 1.0 |
| 4 | `loose(written) == loose(intended)` | `code:form-spacing`（只差空格 / 连字符 / 点） | `FORM_CREDIT["spacing"]` = 0.5 |
| 5 | `loose(written) == loose(heard)` | `code:misheard-kept`（听错的词换了个写法，如 `Cloud.md`） | 0 |
| 6 | 其余 | 交给 `SITE_Q` | 期望得分 |

`loose()` = casefold 后去掉空白、`-`、`_`、`/`，以及**不在两个数字之间**的点。
所以 `Claude md` ≈ `CLAUDE.md`，但 `3.5` ≠ `35`、`C++` ≠ `C`。

三段文本始终取**同一个区间**：required 的 site 向外扩到 actual 编辑碰到的范围，
`written = splice(input[lo:hi], 该区间内的实际编辑)`。这就是 §1.4 第 3 条那个 bug 的修复。

为什么把"形式"收回代码：修完 bug 之后，模型在纯大小写变体上仍然不稳定——
`Readme` 1 级（置信度 0.26）、`readme` 1 级、`ReadMe` 2 级、`Claude.md` 2 级（0.31）。
这类判断按定义就是字符串运算，不该消耗模型的判断力。给多少分是策略问题（§6 第 1 项），不是模型问题。

### 3.3 三个问题（原文）

模型固定为 `jev-1.13.0`。题面用英文：Exp A 对比过中英文题面，准确率相同（各 27/29，样本内），
平均置信度 0.69 对 0.65，差别不足以下结论，原型沿用英文。

**`SITE_Q`** —— state：`heard` / `intended` / `written` 三个句子 + `phrase` 三个短语

```json
{
  "type": "score",
  "instructions": {
    "task": "A speech recognizer misheard a phrase. `heard` is what it produced, `intended` is what the speaker actually said, `written` is what a correction system wrote in that place.",
    "question": "Did the correction system recover the speaker's phrase?"
  },
  "criteria": [
    {
      "what": "Not recovered. `written` still says the misheard phrase, or another wrong word, or a different name than `intended`.",
      "examples": [
        "written keeps the misheard word and only changes its capitalization or punctuation",
        "a person's name spelled with a different character than intended"
      ]
    },
    {
      "what": "Right word, imperfect form. A reader recognizes `written` as the `intended` term, but part of it is missing or it is not written the conventional way.",
      "examples": [
        "a file name with its extension dropped",
        "a product name split into separate lowercase words"
      ]
    },
    {
      "what": "Recovered. `written` is the `intended` term or an equally correct way to write it, and the sentence means what the speaker meant.",
      "examples": [
        "same term with a different but accepted capitalization",
        "a synonym that gives the sentence the same meaning"
      ]
    }
  ]
}
```

**`EDIT_Q`** —— state：`before` / `after`

```json
{
  "type": "choice",
  "instructions": {
    "task": "`before` is dictated speech as accepted by a human reviewer. A correction system changed it into `after`. The system is supposed to fix speech-recognition errors and otherwise keep the speaker's words.",
    "question": "What did the change from `before` to `after` do?"
  },
  "criteria": {
    "fixes_error": {
      "what": "`before` contained a misrecognized or mistyped word, and `after` replaces it with what the speaker evidently said.",
      "not_for": "A word in `before` that already made sense in its sentence."
    },
    "neutral": {
      "what": "Only punctuation, spacing, hyphenation or capitalization changed. Every word and the meaning are the same."
    },
    "rephrases": {
      "what": "`after` says the same thing in different words. The speaker's own wording was replaced, translated or trimmed although it was not an error."
    },
    "damages": {
      "what": "`after` means something different: information was changed, added or removed, or a correct word was replaced by a wrong one."
    },
    "commentary": {
      "what": "`after` contains text that is not part of the speech: a greeting, an explanation, a label, quotation marks or a code fence around it."
    }
  }
}
```

`fixes_error` 是为参考答案自己漏掉的错误准备的：provider 修了一个 `expected` 没修的错，不该被罚。

**`GATE_Q`** —— state：`reference` / `output`

```json
{
  "type": "choice",
  "instructions": {
    "task": "`reference` is the correct transcript of a dictated speech. `output` was produced by a transcript-correction system.",
    "question": "What kind of text is `output`?"
  },
  "criteria": {
    "transcript": "Every sentence of the speech, in order, in the speaker's own words. Some words may be wrong, misspelled or punctuated differently.",
    "incomplete": "A transcript that stops early or leaves out whole sentences of the speech.",
    "extended": "A transcript that also contains sentences the speaker never said, before, inside or after the speech.",
    "not_transcript": "A reply to the speaker, a summary, a rewrite in different words, or a translation."
  }
}
```

### 3.4 计分

- 单点：`credit = Σ P(level) × SITE_CREDIT[level]`，`SITE_CREDIT = [0, 0.5, 1.0]`。用概率的期望，
  不用 argmax，模型犹豫时分数自然落在中间。
- 多改：`credit = Σ P(choice) × EDIT_CREDIT[choice]`，
  `EDIT_CREDIT = {fixes_error: 1.0, neutral: 1.0, rephrases: 0.7, damages: 0, commentary: 0}`；
  `restraint` = 各组 credit 的乘积。
- Gate：`not_transcript` → correction 和 restraint 都归零；`incomplete` / `extended` → restraint 归零。
- 这些数字都是**策略旋钮**，不是模型输出，改它们不需要重新验证问题。

### 3.5 置信度路由与人工钉住

任何模型结论的 `confidence < 0.4` → 进复核清单，随报告打印。你看过之后把结论钉进
`test_cases.toml`，之后同样的 (heard, written) 不再问模型。草案：

```toml
[[case.pin]]              # 单点
heard = "cloud md"
written = "Claude"
credit = 1.0              # 你的检查点描述写的是 "Claude / Claude.md / CLAUDE.md 均可"

[[case.pin]]              # 多改
before = "在查看哪个目录"
after = "再查看哪个目录"
credit = 1.0
```

这样判别器的角色是：**对没见过的写法给出第一判断并标出没把握的**，有争议的最终由你定，
而且只需要定一次。规则表从"预先想全所有写法"变成"事后确认少数几条"。

### 3.6 试过但没有采用的

都是在开发集上、H2 运行之前决定的。

- **收紧版问题 `SITE_Q_V2`**（把问题改成"written 是不是 intended 这个词"，给 1 级加 `not_for`）。
  本来是针对 H1 的"失败"设计的。bug 修掉以后它并不比原问题好：没把握的回答多一倍（44 个里 14–16 个，
  原问题 8 个），出过的严重错误有 `Claude.md`→0 级、`读我`→2 级、`下一层`→0 级、`Voice X`→0 级、
  `voicex`→0 级，置信度都不超过 0.23，其中三个在两次运行之间翻转了（§4.3）。保留在原型里仅供对比。
- **"半对"代码规则**（逐字比对，部分字还是听错的 → 直接判 0）。它是启发式：
  `帐号→账户` 被写成 `账号` 会被它判 0，但那是合理的同义词；而代码结论不进复核清单。
  修掉 bug 以后模型在开发集上自己就能判对这一类，所以没加。H2 的那一个错恰好在这一类，见 §4.5、§6 第 5 项。

## 4. 实验证据

可信度从低到高排：§4.1 是我构造的反例（说明能力，不说明准确率）；§4.3 是开发集（我看着结果做过决定）；
§4.4–4.5 是留出集；§4.2 是全部真实输出。单次实验的 API 花费在 $0.0006–$0.003 之间。

### 4.1 规则 vs 判别器：12 个构造的输出

用例 "Multiple errors"，每个输出的正确答案由构造方式决定。跑了 4 次，结论一致
（脚本：`rules_vs_judge2.py`，日志：`data/rules_vs_judge2.log`）。

| 输出 | 规则 | gate | correction | restraint | 人会怎么判 |
|---|---|---|---|---|---|
| 完美 | 1.00 | transcript | 1.00 | 1.00 | 满分 |
| 原样不动 | 0.00 | transcript | 0.00 | 1.00 | 零分 |
| 同义词 `下一层` | 0.67 | transcript | 0.85–0.89 ⚑ | 1.00 | 修对了 |
| `README.md` | 1.00 | transcript | 0.95–0.96 | 1.00 | 修对了 |
| `Cloud.md`（听错的词换了写法） | 0.67 | transcript | 0.67 | 1.00 | 没修对 |
| 保留 `Cloud.md`，末尾提一句 Claude | **1.00** | transcript ⚑ | 0.67 | **0.00** | 没修对 + 废话 |
| 否定词被改掉 | **1.00** | transcript | 1.00 | **0.03** | 意思被破坏 |
| 结尾编造一段 | **1.00** | extended | 1.00 | **0.00** | 加了没说过的话 |
| 开头加"好的，以下是…" | **1.00** | extended | 1.00 | **0.00** | 不干净 |
| 直接回答而不是纠正 | 0.67 | not_transcript | 0.00 | 0.00 | 彻底失败 |
| 输出摘要 | **1.00** | not_transcript | 0.00 | 0.00 | 彻底失败 |
| 最后一句被润色改写 | 1.00 | transcript | 1.00 | 0.70 | 修对了，但多改了 |

加粗的规则分数都是错的，对应的判别器列都是对的。⚑ = 有低置信结论：`下一层` 的单点置信度
每次都是 0.00（模型在 1 级和 2 级之间对半分，期望得分仍然合理）；"末尾提一句 Claude"那一行的
gate 置信度 0.32–0.38，不过 restraint 由 EDIT 结论归零，gate 怎么判不影响结果。

### 4.2 全部 60 条真实输出

15 个 provider × 2 个用例 × 2 轮（`data/baseline_run.json`）。独立跑 3 次（`expE2.py`，`analyzeE.py v2-1`）。

**调用量。** 60 条输出只产生 19 个不同的问题 = 19 次调用、13,698 tokens、**$0.00058 / 次运行**
（现行方案：60 次调用、约 $0.005）。大部分 site 被代码定了；相同的问题只问一次。

**稳定性。** 3 次运行，19 个结论 **0 次翻转**；provider 级 sd：correction 0.0000，restraint 最大 0.0158。

**方向。** 用例 "Multiple errors"，n = 30，与 §1.1 同一批输出：

| 规则命中 | n | correction | restraint |
|---|---|---|---|
| 0/3 | 2 | 0.000 | 1.000 |
| 1/3 | 12 | 0.181 | 1.000 |
| 2/3 | 10 | 0.583 | 1.000 |
| 3/3 | 6 | 1.000 | 0.607 |

r(规则命中, correction) = **+0.975**（现行的 quality 是 −0.123）。

**模型实际判了什么。** 只有 4 个不同的结论需要模型：

| 内容 | 结论 | 置信度 | 得分 | 涉及 |
|---|---|---|---|---|
| site：`cloud md` → `Claude`（丢了 .md） | 1 级 | 0.97 | 0.50 | ×4，Qwen3.6-Plus、Qwen3.7-Plus |
| edit：`，` → `。` | neutral | 0.56 | 0.88 | ×1，DeepSeek V4 Flash (Volc) |
| edit：`high level` → `high-level` | neutral | 0.72 | 1.00 | ×2 |
| edit：`在查看` → `再查看` | damages | **0.36 ⚑** | 0.44 | ×4，Gemini 3.7 / 3.8 Flash |

代码定的：`Claude.md` ×9 → 1.0，`Claude md` ×10、`Claude MD` ×2 → 0.5，`Readme` / `readme` → 1.0，
`Cloud MD`、`cloud.md` → 0（听错的词换写法）。Gate：60/60 transcript，最小 P = 0.96。

**按 provider：**

| provider | 规则 | correction | restraint |
|---|---|---|---|
| DeepSeek V4 Flash (Volc) | 1.00 | 1.000 | 0.970 |
| Gemini 3.7 Flash | 1.00 | 1.000 | 0.721 |
| Gemini 3.8 Flash | 1.00 | 1.000 | 0.721 |
| Doubao-Seed-2.0-Lite | 0.83 | 0.833 | 1.000 |
| DeepSeek V4 Flash | 0.83 | 0.833 | 1.000 |
| Cerebras Qwen 3.8 27B | 0.75 | 0.750 | 1.000 |
| Qwen3.6-Plus | 0.83 | 0.750 | 1.000 |
| Qwen3.7-Plus | 0.83 | 0.750 | 1.000 |
| Doubao-Seed-2.1-Pro | 0.75 | 0.667 | 1.000 |
| Doubao-Seed-2.0-Mini | 0.67 | 0.583 | 1.000 |
| Doubao-Seed-Evolving | 0.67 | 0.583 | 1.000 |
| Qwen3.8-Flash | 0.67 | 0.583 | 1.000 |
| Gemini 3.5 Flash Lite | 0.67 | 0.583 | 1.000 |
| GLM 5.3 Flash (Official) | 0.67 | 0.583 | 1.000 |
| Qwen3.7-Flash | 0.50 | 0.500 | 1.000 |

provider 级 r(规则, correction) = +0.974；r(规则, restraint) = −0.652。

**必须说清楚的三点：**

1. **在真实数据上，判别器的 correction 基本就是规则的复述。** 两者的全部分歧都来自一个策略旋钮：
   规则让 `Claude md` 和 `Claude` 通过，v2 各给 0.5。这不是判别器"更准"，是"部分正确给几分"
   还没定（§6 第 1 项）。真实输出里没有出现同义词、否定翻转、编造这类规则会判错的情况。
2. **restraint 的区分度几乎全部来自一条被标记的结论。** Gemini 两个型号的 0.721 来自 `在→再`
   （置信度 0.36）。这一条本身有歧义："在查看哪个目录"和"再查看哪个目录"都说得通，
   也可能是参考答案错了。它需要你来定（§6 第 3 项）。除此之外所有 provider 的 restraint 都接近 1。
3. **r(规则, restraint) 为负不是又一次"方向反了"。** 它的含义是：修得最全的几个 provider
   恰好也是手最重的。这是两个不同的维度，本来就该分开报。

### 4.3 开发集：问题 × 代码规则的四种组合

数据：Exp A 的 29 个变体（走真实 diff 管线）+ H1 的 44 个变体。**不是留出集。**
q1 = 原问题，q2 = 收紧版；r1 = v1 的代码规则，r2 = §3.2 的级联
（`dev2.py`，每次 88 次调用，$0.0028）。下表是最终代码上的结果（`data/dev2.log`）：

| 配置 | Exp A | H1 sites | 严重错误 |
|---|---|---|---|
| q1/r1 | 28/29（模型判 17/18） | 44/44（模型判 26/26） | 1 |
| **q1/r2（采用）** | 28/29（模型判 10/11，代码定 18） | 44/44（模型判 17/17，代码定 27） | 1 |
| q2/r1 | 28/29 | 43/44 | 2 |
| q2/r2 | 28/29 | 44/44 | 1 |

"严重错误" = 该判 0 的给了分，或不该判 0 的判了 0。四种配置里**每一个错误结论都被标记了**（置信度 < 0.4）。
q1 唯一的错：`read me` → `读我`，模型在三个级别之间几乎均分（置信度 0.00–0.02 ⚑），期望得分约 0.5。

这张表跑过两次。第一次（`data/dev2_with-mix-rule.log`，当时 r2 里还带着后来去掉的"半对"规则）
q2 明显更差：q2/r1 是 26/29 + 42/44、5 个严重错误，q2/r2 是 27/29 + 44/44、2 个。不采用 q2 的决定
是基于第一次的结果、在 H2 之前做的。第二次 q2 的错误少了，原因是它的错全是置信度 ≈ 0 的抛硬币：

| 两次运行对比（同一批 44 个由模型判的 site） | q1 | q2 |
|---|---|---|
| 级别翻转 | 1（`读我`，两次都被标记） | 3（`Claude.md`、`读我`、`Voice X`，置信度都是 0.00） |
| 平均 \|Δ得分\| / 最大 | 0.012 / 0.045 | 0.013 / 0.080 |
| 置信度 < 0.4 的结论数 | 8 / 8 | 14 / 16 |
| 平均置信度 | 0.68 / 0.66 | 0.60 / 0.59 |

所以对 q2 的准确说法是：**准确率不比 q1 好，没把握的回答多一倍**，没有采用的理由。
同时这也是"用期望得分而不是 argmax"的依据：级别会在接近均分时翻转，期望得分两次之间最多差 0.045。

修完 bug 后 q1 对"只改对一半"的判定（最终代码，全部由模型判）：`返力` 0 级 @0.98、`反利` 0 @0.46、
`张玮` 0 @0.70、`章伟` 0 @0.96；相近的几类：`章纬` 0 @0.98、`杭洲` 0 @0.86、`实时数据` 0 @0.35 ⚑、`徐峻博` 0 @0.98。

### 4.4 留出集 H1

标注先于运行写死。Gate 和 EDIT 两个问题的结果有效，v2 没有改动它们：

- **Gate：13/13。** 包括中间缺一句、缺最后一句、编造结尾、加前言、回复、摘要、翻译。
  判对的最低置信度 0.81。
- **多改分类：13/13**（另有 1 条 `二十→20` 是策略题，没设标准答案，模型判 neutral @0.64）。
  包括 `上午→下午`、`先别合并→先合并`、删掉一个分句、整体翻译成英文、加引号、加标签；
  以及两条"参考答案漏掉的错"（`掩饰→演示`、`底→低`）被正确判为 `fixes_error`。
- **单点：39/44 → 修 bug 后 44/44。** 39/44 是 §1.4 第 3 条的 bug；44/44 是看过结果之后得到的，
  只能算开发集。

### 4.5 留出集 H2

针对修好的管线重新做的。5 个新句子、13 个 site、74 个变体，覆盖：人名错字、形近词、
同类产品名、同义词 / 别名、截断、拼写错误、只改对一半、大小写 / 空格变体。
运行前冻结：标注、`expH2.py` / `adjudicate2.py` / `sitebench.py` 的 sha1（`data/expH2_frozen.sha1`）、
主配置 q1/r2、通过标准。**只跑了一次，之后没有调过任何东西。**

预先登记的标准：主配置 ① 没有未被标记的严重错误，且 ② 模型判定部分 ≥ 90% 落在标注内，
才算"可以接管档位"。

| 配置 | 总计 | 模型判定 | 代码定 | 严重错误 |
|---|---|---|---|---|
| q1/r2（主） | **73/74** | **35/36（97%）** | 38/38 | 1，未被标记 |
| q1/r1（次） | 73/74 | 45/46 | 28/28 | 同一个 |

46 次调用，32,316 tokens，$0.0014。

**唯一的错：** `保险` → `保销`（应为 `报销`，只改对了第二个字）判 1 级，置信度 0.46——
高于 0.4 的复核线，所以没被标记。② 通过，① **没通过**。

判对的，按类别（括号里是置信度）：

- 人名错字，全部 0 级：李尉然 (0.99)、李蔚冉 (0.97)、黎蔚然 (0.85)、李薇然 (0.98)、
  欧阳婧 (0.99)、欧阳晶 (1.00)、欧杨靖 (0.95)、幕尼黑 (0.75)
- 形近但不同的词，全部 0 级：报告 (0.98)、异样 (0.93)、建议 (0.67)、线下 (1.00)
- 同类的另一个产品，全部 0 级 (0.99–1.00)：Reddit、Memcached、Kibana、package.json、JavaScript、Vuex、Webpack、V8
- 同义词 / 别名，全部 2 级：报账 (0.67)、不同意见 (0.87)、反对意见 (0.90)、生产 (0.43)、TS (0.72)、Redis 数据库 (0.90)
- 截断，1 级：tsconfig (0.98)、蔚然 (0.83)
- 拼写错误：Radis 0 级 (0.58)、Pina 1 级 (0.61)、Vit 1 级 (0.96)、Graphana 0 级 (0.06 ⚑)
- 只改对一半：报险 0 级 (0.83)、意议 0 级 (0.70)、异义 0 级 (0.00 ⚑)、**保销 1 级 (0.46) ✗**

"只改对一半"这一类合计 7/8（H1 开发集 4/4，H2 3/4）。8 条里有 3 条置信度在 0.5 以下：
`反利` 0.43–0.46（判对）、`保销` 0.46（判错）、`异义` 0.00（判对，已标记）。这是目前唯一看得出规律的薄弱类别。

### 4.6 已知薄弱点

- **只改对一半的中文词**：见上。没被标记的两条不稳结论是 `反利`（0.43–0.46，判对）和 `保销`（0.46，判错）；
  把复核线提到 0.5 能同时拦住它们，但这是看了 H2 之后才有的想法，不能算验证过。
- **同义词判得对，但置信度低**：`下一层` 0.00、`返点` 0.11、`生产` 0.43。会进复核清单，钉一次即可。
- **跨语言的"翻译"**：`read me` → `读我`（标注为 0 级）两次运行分别被判 1 级和 2 级，置信度 0.00–0.02，两次都被标记。
- **数字格式（ITN）**：`二十→20`、`三点五→3.5` 被判 neutral。算不算"多改"是策略问题（§6 第 4 项）。
- **纯标点改动**一律 neutral，包括把标点删光的情况，目前没有单独处理。
- **`在→再`** 这类两边都说得通的改动，模型给 damages + 低置信，需要人定。
- **整个 bench 只有 2 个用例、4 个检查点。** 上面所有关于 provider 排名的结论都建立在这 4 处上。
  这是比判别器更大的瓶颈（§5 第三期）。

## 5. 集成方案与分期

综合分现在的权重（`config.toml`）：basic 0.25 · cloud 0.15 · bonus 0.15 · latency 0.35 · quality 0.10。
三个档位由规则检查点喂，quality 由判别器喂。

### 第一期：判别器只管规则看不到的东西（建议现在做）

1. **`quality := 10 × restraint`**（含 gate）。restraint 按构造就与规则正交：规则看"该改的改了没"，
   它看"不该动的动了没"。支撑它的两个问题在留出集上各 13/13，在 60 条真实输出上 3 次运行 0 翻转。
2. **规则继续喂 basic / cloud / bonus 三个档位**，排名口径不变。
3. **判别器的 correction 只打印、不计分**：和规则命中并排显示，列出逐 site 的分歧和复核清单。
   这是给第二期攒证据，也让你能直接看到判别器在新用例上的表现。
4. **修掉 I1–I8**：相同问题只问一次（去重 + single-flight）；缺键 / 题型不符直接报错，不再静默给 0；
   429 / 529 退避重试；判别器失败单独计数并全部列出，不混进 provider 的成绩；逐条结论（state 摘要、
   概率、置信度、来源是代码还是模型）写进运行结果 JSON；删掉写死的 "r≈0.89"。
5. **加序关系测试**（不需要网络，用录制的回答）：完美输出 ≥ 原样输出；完美输出 ≥ §4.1 里每一个被破坏的输出。
   §1.1 的问题就是缺这一条测试。
6. 退役 `typesafe_questions.json` 里的 v3 题库（fidelity / fluency / asr_fix / clean_output）和对应的
   `quality_*` 配置项。v3 从未提交过，所以是直接替换，不需要兼容旧配置。三个新问题仍放在 JSON 文件里可编辑，但加载时校验题型和选项键。

一个需要你知道的后果：在现在这 2 个用例上，几乎所有 provider 的 restraint 都是 1.0（§4.2），
quality 这一维会很平。这是如实反映——现有数据里确实没人乱改。它的价值是兜底：一旦某个 provider
加前言、编内容、答非所问、改掉否定词，规则给 1.00，它给 0。

可选的加强（要你定）：gate 判为 `not_transcript` 时，把该输出的规则档位也清零。理由是一段摘要里
碰巧出现 "Claude" 和 "README" 不算"修对了"（§4.1 倒数第二行，规则 1.00）。Gate 在留出集 13/13、
真实数据 60/60（最小 P 0.96），我认为证据够；但它会让判别器第一次影响档位，所以单列出来。

### 第二期：判别器接管档位（有前提）

- 配置开关 `checkpoint_source = "rules" | "judge"`，默认 `rules`。
- `judge` 模式下三个档位由 site 的分级得分喂；每个 site 属于哪个档位，通过把现有检查点定位到
  diff site 上得到（或者在用例里直接标）。
- **前提：** ① 先定 §6 第 5 项（"只改对一半"怎么处理），然后在一个新的留出集（H3）上过预先登记的标准——
  H2 离过线只差一条，但差一条就是没过，而且 H2 已经看过了，不能再用；② 第一期跑出来的复核清单
  你审过并钉住。

### 第三期：加用例

整个 bench 只有 2 个用例、4 个检查点，这比判别器本身更限制结论的可靠性。新方案下写一个用例只需要
`input` + `expected`，不用再为每个点设计 `must_contain` / `must_not_contain`，所以加用例的成本低了很多。
建议优先补：人名 / 专有名词、同音词、中英混排的产品名、数字、长段落里的单点错误，
以及"输入本来就没错"的用例（专门测 restraint）。

### 移植注意事项

- 原型用 Python `difflib.SequenceMatcher` 在 token 序列上做 diff（拉丁词整词、中文逐字，编辑区间沿拉丁词
  向外扩，所以 `cloud md` → `CLAUDE.md` 是一处编辑）。Rust 里无论用 `similar` 还是自己写 LCS，
  对齐结果在有歧义时可能与 difflib 不同。**验收标准**：Rust 版对 H1、H2、60 条真实输出产生的
  (heard, intended, written) 三元组与原型逐条一致——离线、不花钱（`expH2.py dry` 就是干这个的）。
- 纯 Rust、无平台相关代码，macOS / Windows 行为一致。
- 如果只做第一期，需要移植的是 diff、EDIT 分组、GATE 和客户端；`settle()` 和 `SITE_Q` 只用于打印。

## 6. 需要你拍板的策略项

这些都不是模型能回答的问题，是"我们想要什么"的问题。

1. **"词对了、形式不完美"给几分。** 现在 `FORM_CREDIT = {case: 1.0, spacing: 0.5}`，模型判的 1 级也是 0.5。
   这一项影响很大：用例 2 的 30 条输出里**没有一条**写出了 `CLAUDE.md`——`Claude md` ×10、`Claude.md` ×9、
   `Claude` ×4、`Claude MD` ×2，其余是没修对的。你在检查点描述里写的是 "Claude / Claude.md / CLAUDE.md 均可"，
   按这个口径这一处应该全给 1.0。两种做法：把 spacing 和 1 级的得分整体调到 1.0（宽松）；
   或保持 0.5，只对这一个 site 加钉子（§3.5）。我倾向后者：丢了扩展名的文件名一般不该拿满分，
   这个 site 是特例。
2. **低置信结论怎么计分。** 建议：site 用期望得分；多改的地方在你钉住之前按期望得分算、同时标记。
   另一个选择是"未钉住的低置信结论不扣分"，对 provider 更宽容，但会让 `在→再` 这类白白过关。
3. **三条现成的待钉结论**：`在查看→再查看`（Gemini 两个型号的 restraint 全靠它）、`cloud md→Claude`、
   `下一集→下一层`。
4. **数字格式（ITN）**：`二十→20`、`三点五→3.5` 算 neutral 还是算多改？现在模型判 neutral。
5. **"只改对一半"怎么处理**（H2 唯一的错）。三个选择：
   (a) 保持现状，交给模型，接受 8 条里错 1 条；
   (b) 把"逐字比对后是 heard / intended 的混合"当作**路由信号**：照常问模型，但无论置信度多少都进复核清单；
   (c) 恢复成代码规则直接判 0，个别同义词误伤（`账号`）靠钉子解决。
   我倾向 (b)：不让启发式直接定分，但保证这一类一定有人看。(b)、(c) 都还没有在留出集上验证过，
   要和第二期的 H3 一起验。
6. **复核线 0.4 要不要提到 0.5。** H2 上能多拦住那条错的，代价是复核清单变长；同样属于事后想法，要在 H3 上验。

## 7. 复现

全部材料在 `.local-notes/llm-bench-judge-v4/`（gitignore，不入库），从该目录运行 `uv run python <脚本>`。
API key 从 `tools/llm-bench/.jev_key` 读取，脚本不会打印它。文件清单和每个脚本是否调用 API 见该目录的 `README.md`。

| 本文章节 | 脚本 | 数据 | 调 API |
|---|---|---|---|
| §1.1 | `shipped_inversion.py` | `data/ts_scores.json`、`data/baseline_run.json` | 否 |
| §4.1 | `rules_vs_judge2.py` | `data/rules_vs_judge2.log` | 是（约 $0.001） |
| §4.2 | `expE2.py <tag>` → `analyzeE.py v2-<tag>` | `data/expE_v2-{1,2,3}.json` | 前者是（$0.0006） |
| §4.3 | `dev2.py` | `data/dev2.{json,log}`、`data/dev2_with-mix-rule.{json,log}` | 是（$0.0028） |
| §4.4 | `expH.py` | `data/expH_1.json`（site 部分受 v1 bug 影响，见 `seen` 字段） | 是 |
| §4.5 | `expH2.py dry` / `run <tag>` | `data/expH2_1.{json,log}`、`data/expH2_frozen.sha1` | `run` 是（$0.0014）。**已跑过一次，不要对着它调** |
| 原型 | `adjudicate2.py`（v2）、`adjudicate.py`（v1，含 bug，保留备查） | — | — |

## 8. 补记（2026-09-20 晚）：用户裁定与方案收敛

**裁定。**

- bench 要看的是三件事：① 按词典把同音词改对；② 速度；③ 词典里没有、靠语义才能改对的错（如 `下一集→下一级`）。
- `cloud md` 这一处，`Claude` / `Claude md` / `Claude.md` / `CLAUDE.md` **都算对**——要求只是把 `cloud`
  换成词典里的 `Claude`。§6 第 1 项就此关闭；我原先"保持 0.5、单独钉这一处"的倾向被否决。

**由此得到的离线结论**（`dict_split.py`，不调 API）。按这个口径重算 60 条真实输出：correction 与手写规则的
命中数 **60/60 完全相同**，而且**没有任何一个 site 需要问模型**。此前两者的全部分歧都来自我设的 0.5。
也就是说：在现有数据上，模型对"修没修对"的贡献是零——四个检查点里两个是词典词（一个人名、`Claude`
都在词典里），答案是已知字符串，代码判就是精确的；另外两个要么被一字不差地改对，要么没动。

**收敛后的方案**（取代 §5 的三期划分，待确认）：

1. site 由 `diff(input → expected)` 得到。用例只写 `input` + `expected`，不再手写 `must_contain` / 正则。
   取代手写规则的是 diff + 词典，不是模型。
2. **词典 site**（`expected` 在该处含有词典词；词典已经在 config 里）：输出在这个位置出现该词典词即通过，
   大小写、后缀不限；否则不通过。纯代码。对应 ①。判定锚定在位置上，所以"保留 `Cloud.md`、末尾提一句 Claude"不再过关。
3. **其余 site**：与 `expected` 一致或只差大小写 → 通过；原样没动 → 不通过；**其他写法**（`下一层` 之类）→
   问 `SITE_Q`，低置信进复核清单，人工钉住后不再问。对应 ③。模型只在这里参与计分。
   "只改对一半"这一类（§4.5 唯一的错）无论置信度多少都进复核清单。
4. 输出如果只动了 site，按构造就是一份干净的转写，不问模型；动了别处才问 `EDIT_Q` / `GATE_Q`。
   这一项取代 v3 的 quality 题库（fidelity / fluency / asr_fix / clean_output 全部退役）。
5. 档位按 site 的来源自动归类：`dictionary`（原 basic + cloud，权重合并为 0.40）/ `semantic`（原 bonus 0.15）/
   `latency` 0.35 / `clean`（原 quality 0.10）。速度维度不变，判别器在计时之外运行。
6. 没有 API key 或 `--skip-judge` 时 bench 照常出分；需要模型而没问到的 site 单独列为"未判"，不静默记 0。
7. I1–I8 的修复和序关系测试（§5 第一期第 4、5 项）照做。

移植时注意：词典 site 要扩到它所覆盖的整个词典词（人名同音字的 diff 只有一个字，词典词是整个三字人名）；
当 `heard` 与 `intended` 去掉空格大小写后相同（`read me` / `README`）时，形式本身就是要改的内容，不能套用"只差空格"的宽松比较。

## 9. 实现记录（2026-09-20）

§8 的方案已在 `tools/llm-bench` 落地（未提交）：

- `src/adjudicate.rs`：纯代码部分——由 `diff(input → expected)` 得到 site、按词典分档、逐处判定、
  site 之外的改动归类（空格/句读/大小写由代码判为中性，其余成组提问）、计分。不联网，测试全部离线，
  含 §4.1 的 12 个输出的序关系测试。
- `src/typesafe.rs`：判别器客户端。每个不同的问题只问一次（I1）、答案不完整即报错而不是取默认值（I2、I6）、
  429/529/传输错误按 1/2/4 秒重试（I4）。三个问题在 `typesafe_questions.json`，启动时校验选项与计分代码一致。
- `src/main.rs`：计时循环里只跑代码判定；全部模型跑完后统一问判别器，再计分。失败的问题连同错误打印（I3），
  置信度 < 0.4 的裁决进复核清单（I5），每一轮的逐处裁决写进 `--output` 的 JSON（I7）。
  v3 题库、自由格式 LLM 评委、`[[case.checkpoint]]` 和 `regex` 依赖一并移除。
- 用例只有 `name` / `input` / `expected`，可选 `[[case.pin]]`（`heard` / `written` / `credit`）作为人工终裁；
  旧的 `[[case.checkpoint]]` 会在加载时报错而不是被忽略。
- `[eval]`：`weight_dictionary` 0.40 / `weight_latency` 0.35 / `weight_semantic` 0.15 / `weight_clean` 0.10。
  旧名字继续可用（`weight_basic + weight_cloud` → dictionary，`weight_bonus` → semantic，`weight_quality` → clean）；
  未知的键现在报错。综合分只在该模型有得分率的维度上加权平均：没问到的裁决标 `*`、不计入得分率，也不记 0。
- 调用失败的那一轮，在该用例的每一处和 clean 上都记 0。

验证：`cargo test` 30 项通过；用保存的 60 条真实输出离线回放，只有 4 条需要判别器（2 个不同的问题）；
真实配置端到端跑一轮（16 个模型 × 2 个用例）：8 处裁决需要判别器，合并为 6 次调用，0 失败，约 4.5k 输入 token。
H2 没有重跑；`SITE_Q` 的措辞没有再动。以后若要改问题或代码规则，需要一份新的冻结留出集。
