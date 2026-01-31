# Email Classifier Project Specification

## Contents

| File | Description |
|------|-------------|
| **SPEC.md** | Main specification document - **start here** |
| AUTH_SETUP.md | Step-by-step Azure AD app registration guide |
| CONTENTS.md | This file |

## Quick Start

1. Read `SPEC.md` for the full system design
2. Follow `AUTH_SETUP.md` to register your Azure AD app
3. Start building with Claude Code

## Project Summary

A Rust application that:
- Fetches email from Office 365 via Microsoft Graph API
- Stores locally in SQLite (no attachments)
- Embeds emails using Ollama (nomic-embed-text)
- Classifies using Qwen via retrieval-augmented few-shot prompting
- Notifies user of important emails
- Learns from user corrections without retraining

## Key Design Decisions

- **No RLHF needed**: Retrieval-augmented few-shot learning handles the feedback loop. User corrections become high-quality examples that immediately influence future classifications.

- **SQLite for everything**: Emails, metadata, labels, and vectors (via sqlite-vec). Simple, no external services, easy backup.

- **Ollama for inference**: You already have it running. Uses nomic-embed-text for embeddings, qwen2.5:7b for classification.

- **No attachment content**: Just metadata (has_attachments, filenames). Keeps storage small, classification fast.

## Hardware Target

- NVIDIA RTX 3090 (24GB VRAM)
- Dual Xeon, 64GB RAM
- Windows (work machine)
- Ollama already running
