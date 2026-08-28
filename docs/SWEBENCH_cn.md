# SWE-bench

CodeSmith 的 SWE-bench 适配器会写出官方 SWE-bench 评测 harness 所期望的
预测文件。它并不取代 harness 本身；它的作用是从本地任务工作区生成
`model_patch` 行。

## 单个实例

从一个检出至 SWE-bench 实例基础提交（base commit）的工作区开始，并将
issue 文本保存到本地：

```bash
codesmith swebench run \
  --instance-id django__django-12345 \
  --issue-file issue.md \
  --predictions-path all_preds.jsonl
```

`run` 会调用工具支撑的非交互模式，等价于 `codesmith exec --auto`，默认
输出 `stream-json`。回合结束后，CodeSmith 会把
`git diff --binary --no-ext-diff` 导出为一条 JSONL 预测行：

```json
{"instance_id":"django__django-12345","model_name_or_path":"codesmith/deepseek-v4-pro","model_patch":"diff --git ..."}
```

如果你已经运行过 CodeSmith，或者手动编辑过工作区，可以不再进行模型回合，
直接导出当前 diff：

```bash
codesmith swebench export \
  --instance-id django__django-12345 \
  --predictions-path all_preds.jsonl
```

这两条命令都会更新同一 `instance_id` 对应的行，而不是追加重复行。导出
diff 前会先用 `git add -N` 标记未跟踪文件，使新建的文件出现在补丁中。

## 评测

按照 SWE-bench 官方安装说明安装 SWE-bench 和 Docker，然后把预测文件传给
官方 harness：

```bash
python -m swebench.harness.run_evaluation \
  --dataset_name princeton-nlp/SWE-bench_Lite \
  --predictions_path all_preds.jsonl \
  --max_workers 1 \
  --run_id codesmith-smoke
```

在 Apple Silicon 上，SWE-bench 官方文档建议加上 `--namespace ''`，让
镜像在本地构建而不是拉取 Linux 镜像。

## 批处理驱动器的形态

一个简单的批量运行器应当：准备每个实例的工作区、把 issue 正文写入
`issue.md`、运行 `codesmith swebench run`，然后对累积的 `all_preds.jsonl`
调用一次 harness。

为了保证运行可复现，需固定：

- CodeSmith 版本与提交：`codesmith --version`
- 模型标签：`--model-name-or-path codesmith/deepseek-v4-pro`
- harness 使用的数据集与数据切分
- Docker 平台与 worker 数量
- `all_preds.jsonl` 文件与 CodeSmith 流式日志

官方参考：

- SWE-bench 仓库：https://github.com/SWE-bench/SWE-bench
- SWE-bench harness 文档：https://www.swebench.com/SWE-bench/api/harness/
