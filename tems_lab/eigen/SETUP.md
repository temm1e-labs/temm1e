# Eigen-Tune Setup Guide

Get self-tuning running on your machine. Takes 5 minutes.

---

## Prerequisites

Eigen-Tune needs two external tools. TEMM1E itself is pure Rust — these are only for the training and serving pipeline.

### 1. Ollama (Required — serves the fine-tuned model)

Ollama runs your fine-tuned model locally. Without it, Eigen-Tune still collects training data but cannot train or serve models.

**macOS:**
```bash
brew install ollama
ollama serve
```

**Linux:**
```bash
curl -fsSL https://ollama.com/install.sh | sh
ollama serve
```

**Windows:**
Download from https://ollama.com/download

**Verify:**
```bash
curl http://localhost:11434/
# Should print: "Ollama is running"
```

### 2. MLX (macOS Apple Silicon) or Unsloth (NVIDIA GPU) — for training

You need ONE of these, depending on your hardware.

**Apple Silicon (M1/M2/M3/M4):**
```bash
# Requires Python 3.10+
python3 -m pip install mlx-lm

# Verify:
python3 -c "import mlx_lm; print('MLX ready')"
```

If your system Python is too old, use Homebrew:
```bash
brew install python@3.12
python3.12 -m venv ~/.eigentune-env
source ~/.eigentune-env/bin/activate
pip install mlx-lm
```

**NVIDIA GPU (Linux/Windows):**
```bash
pip install unsloth

# Verify:
python3 -c "import unsloth; print('Unsloth ready')"
```

**No GPU?** Eigen-Tune still collects and scores training data. When you get access to a GPU (or a cloud GPU burst), the data is ready to train.

---

## Enable Eigen-Tune

Add to your `temm1e.toml`:

```toml
[eigentune]
enabled = true
```

That's it. Restart TEMM1E and the system begins collecting training data from every conversation.

---

## Choose a Base Model

Eigen-Tune needs a base model to fine-tune. You pick the model. Ollama is the model registry — whatever you pull into Ollama is available for Eigen-Tune.

### Step 1: Browse and pull a model

Visit [ollama.com/library](https://ollama.com/library) to see all available models. Then pull one:

```bash
# Small (8 GB RAM) — fast training, good for testing
ollama pull smollm2:135m
ollama pull qwen2.5:0.5b

# Medium (16 GB RAM) — good balance
ollama pull qwen2.5:1.5b
ollama pull phi3.5:3.8b
ollama pull llama3.1:8b

# Large (32 GB+ RAM) — best quality
ollama pull mistral-small:24b
ollama pull qwen2.5:32b
```

Any model Ollama supports works. When new models release (Llama 4, Qwen3, etc.), just `ollama pull` them — no TEMM1E update needed.

### Step 2: Set the model in TEMM1E

```
/eigentune model                    Show what's available
/eigentune model llama3.1:8b        Set a specific model
/eigentune model auto               Let system pick based on your hardware
```

Or in `temm1e.toml`:

```toml
[eigentune]
enabled = true
base_model = "llama3.1:8b"     # any model name from ollama list
```

### Step 3: There is no step 3

The system handles everything else. Your model choice only affects what base model gets fine-tuned. The training data, quality scoring, graduation gates, and monitoring are all automatic.

### Recommendations by hardware

| RAM | Model | Training time (1000 examples) | Inference |
|-----|-------|------|-----------|
| 8 GB | smollm2:135m | ~10 min | ~200 tok/s |
| 8 GB | qwen2.5:0.5b | ~20 min | ~150 tok/s |
| 16 GB | qwen2.5:1.5b | ~40 min | ~80 tok/s |
| 16 GB | llama3.1:8b | ~2 hours | ~30 tok/s |
| 32 GB | mistral-small:24b | ~4 hours | ~15 tok/s |
| 48 GB+ | qwen2.5:32b | ~6 hours | ~10 tok/s |

Bigger models produce better results but train slower and run slower. Start small, upgrade when you have the data to justify it.

### Important: Ollama IS the model registry

Eigen-Tune does not maintain its own model list. Ollama is the source of truth. This means:

- **New models:** `ollama pull <new-model>` makes it instantly available. No TEMM1E update required.
- **Custom models:** If you have a GGUF file from any source, import it via `ollama create mymodel -f Modelfile`. Eigen-Tune can fine-tune it.
- **Model updates:** `ollama pull llama3.1:8b` always gets the latest version. Re-run training to use it.
- **No lock-in:** Switch models anytime with `/eigentune model <name>`. Previous training data is preserved and can be used with the new model.

---

## Check Status

```
/eigentune status             Full status: data, tiers, model, prerequisites
/eigentune model              Model selection and hardware info
```

---

## What Happens Next

1. **Collecting** — every conversation produces training pairs, automatically scored
2. **Training** — when enough quality data accumulates (500+ pairs per tier), training triggers automatically
3. **Evaluating** — the trained model is tested against your actual conversation patterns
4. **Graduating** — if it passes statistical gates (95% accuracy, 99% confidence), it starts serving simple queries locally
5. **Monitoring** — continuous drift detection, auto-demotion if quality drops

You don't need to do anything. The system handles the entire lifecycle. Cloud is always the fallback — if anything goes wrong, your experience is identical to before Eigen-Tune existed.

---

## Troubleshooting

**"No training backend available"**
- Install MLX (Apple Silicon) or Unsloth (NVIDIA GPU)
- Eigen-Tune still collects data without a training backend

**"Ollama not running"**
- Run `ollama serve` in a separate terminal
- Or set up as a system service: `brew services start ollama` (macOS)

**"Python not found" or "mlx_lm not found"**
- Ensure Python 3.10+ is installed
- If using a venv, activate it before starting TEMM1E
- Or install globally: `pip install mlx-lm`

**"Not enough data"**
- Eigen-Tune needs ~500 quality conversations per tier before first training
- Keep using TEMM1E normally — data accumulates automatically
- Check progress: `/eigentune status`

---

## What Eigen-Tune Does NOT Do

- Does NOT send your data anywhere — all training is local
- Does NOT require GPU for data collection — only for training
- Does NOT modify your existing conversations or provider behavior
- Does NOT cost any additional LLM API money
- Does NOT replace your cloud provider — it gradually supplements it for simple queries
