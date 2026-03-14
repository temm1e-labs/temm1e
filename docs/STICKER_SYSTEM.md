# Temm1e Sticker System — Design Doc

## Overview

Give Temm1e the ability to send contextually appropriate stickers from a custom pixel art expression sheet.

## Source Asset

- Sprite sheet: `assets/stickers/spritesheet.png` (100-expression pixel art)
- Needs: cutting into individual stickers, background removal, labeling

## Architecture

### Single `sticker` Tool (No Toolbox Pollution)

One tool, not 100. The LLM sees a single `sticker` tool with curated emotion labels.

```json
{"emotion": "happy"}
```

### Components

1. **Sprite Processing** (Python script, one-time)
   - Cut grid into individual PNGs
   - Remove gray background -> transparent
   - Resize to 512x512 (Telegram requirement)
   - Export as WebP

2. **Manifest** (`assets/stickers/manifest.json`)
   ```json
   {
     "happy": {"file": "sticker_01.webp", "emoji": "😊"},
     "confused": {"file": "sticker_12.webp", "emoji": "😕"},
     "sleeping": {"file": "sticker_15.webp", "emoji": "😴"},
     "error": {"file": "sticker_error.webp", "emoji": "❌"},
     "success": {"file": "sticker_success.webp", "emoji": "✅"}
   }
   ```

3. **Telegram Sticker Set** (via Bot API `createNewStickerSet`)
   - Each sticker gets a `file_id` for instant sending
   - No re-upload needed after initial registration

4. **Rust Tool** (`crates/temm1e-tools/src/sticker.rs`)
   - Loads manifest at startup
   - Tool description lists available emotions
   - Returns `StickerOutput` handled by Telegram channel as `sendSticker`

5. **Channel Support**
   - `OutboundMessage::Sticker { file_id }` variant
   - Telegram channel: `send_sticker(chat_id, file_id)`

### System Prompt Guidance

> You can express yourself with stickers using the `sticker` tool. Use them like a real person would — to punctuate emotions, not every message. Available: happy, confused, sleeping, thinking, error, success, etc.

## Sticker Categories (Draft — ~25 curated from 100 sprites)

| Emotion | Description | Row/Col in sheet |
|---------|-------------|------------------|
| happy | Default happy face | R1C1 |
| excited | Extra happy, sparkles | R3C1 |
| confused | Question mark | R2C2 |
| thinking | Contemplative | R3C4 |
| sleeping | Zzz | R2C1 |
| sad | Teary eyes | R1C9 |
| angry | Upset face | R1C8 |
| surprised | Wide eyes | R2C7 |
| proud | Crown | R4C1 |
| reading | With book | R4C4 |
| smart | With glasses | R3C6 |
| eating | With bone/treat | R2C8 |
| error | ERROR badge | R4C8 |
| success | SUCCESS badge | R4C7 |
| save | SAVE badge | R4C6 |
| level_up | Level Up! badge | R4C9 |
| achievement | Trophy | R5C7 |
| new_game | New Game badge | R5C6 |
| playing | With ball | R5C4 |
| waving | Greeting pose | R1C3 |

## Status

- [ ] Cut sprite sheet into individual stickers
- [ ] Remove background, make transparent
- [ ] Label and create manifest
- [ ] Register Telegram sticker set
- [ ] Build `sticker` tool
- [ ] Add `OutboundMessage::Sticker` variant
- [ ] Wire Telegram `sendSticker`
- [ ] Add system prompt guidance
- [ ] Test contextual sticker sending
