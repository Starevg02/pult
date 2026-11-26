// Свой статус: режим присутствия (как в мессенджерах) и свой текст с эмодзи и сроком.
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import type { MyCard } from "../../api/client";
import { formatDate } from "../../format";
import { Button, Dialog, Field, Input, Select } from "../../ui";
import { PresenceDot } from "./presence";
import { MODES, useSaveStatus } from "./profileHooks";

const MODE_DOT = { auto: "online", away: "away", dnd: "dnd", invisible: "offline" } as const;

/** Выбор режима присутствия — строки меню учётной записи. */
export function PresencePicker({ card, onDone }: { card: MyCard; onDone?: () => void }) {
  const { t } = useTranslation();
  const save = useSaveStatus();
  return (
    <div role="group" aria-label={t("people.presence.label")}>
      {MODES.map((m) => (
        <button
          key={m}
          type="button"
          aria-pressed={card.presence === m}
          disabled={save.isPending}
          className={`flex min-h-9 w-full items-center gap-2.5 rounded-lg px-2 text-left text-sm hover:bg-hover ${card.presence === m ? "font-medium" : ""}`}
          onClick={() =>
            save.mutate(
              { presence: m, status_emoji: card.status_emoji, status_text: card.status_text, status_until: card.status_until },
              { onSuccess: onDone },
            )
          }
        >
          <PresenceDot presence={MODE_DOT[m]} className="h-2.5 w-2.5" />
          <span className="flex-1">{t(`people.mode.${m}`)}</span>
          {card.presence === m && <span aria-hidden="true">✓</span>}
        </button>
      ))}
    </div>
  );
}

const PRESETS = [
  ["🏖", "vacation"],
  ["🤒", "sick"],
  ["🏠", "remote"],
  ["📅", "meeting"],
  ["🍽", "lunch"],
  ["🚗", "onTheRoad"],
] as const;

const CLEAR_AFTER = ["never", "30m", "1h", "4h", "today", "week"] as const;
/** `keep` — оставить срок, который уже стоит. */
type ClearAfter = (typeof CLEAR_AFTER)[number] | "keep";

function untilOf(c: ClearAfter, current: string | null | undefined): string | null {
  const now = new Date();
  switch (c) {
    case "keep":
      return current ?? null;
    case "never":
      return null;
    case "30m":
      return new Date(now.getTime() + 30 * 60_000).toISOString();
    case "1h":
      return new Date(now.getTime() + 60 * 60_000).toISOString();
    case "4h":
      return new Date(now.getTime() + 4 * 60 * 60_000).toISOString();
    case "today": {
      const end = new Date(now);
      end.setHours(23, 59, 59, 0);
      return end.toISOString();
    }
    case "week":
      return new Date(now.getTime() + 7 * 24 * 60 * 60_000).toISOString();
  }
}

/** Свой статус: эмодзи, текст, когда очистить. */
export function StatusDialog({ card, open, onClose }: { card: MyCard; open: boolean; onClose: () => void }) {
  const { t, i18n } = useTranslation();
  const save = useSaveStatus();
  const [emoji, setEmoji] = useState(card.status_emoji ?? "");
  const [text, setText] = useState(card.status_text ?? "");
  const [clear, setClear] = useState<ClearAfter>(card.status_until ? "keep" : "never");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    save.mutate(
      { presence: card.presence, status_emoji: emoji.trim() || null, status_text: text.trim() || null, status_until: untilOf(clear, card.status_until) },
      { onSuccess: onClose },
    );
  };
  return (
    <Dialog open={open} onClose={onClose} title={t("people.status.title")}>
      <form onSubmit={submit} className="space-y-4">
        <div className="flex flex-wrap gap-1.5">
          {PRESETS.map(([e, k]) => (
            <button
              key={k}
              type="button"
              className="min-h-8 rounded-full border border-line px-2.5 text-sm hover:bg-hover"
              onClick={() => {
                setEmoji(e);
                setText(t(`people.status.preset.${k}`));
              }}
            >
              {e} {t(`people.status.preset.${k}`)}
            </button>
          ))}
        </div>
        <div className="flex gap-2">
          <Field label={t("people.status.emoji")}>
            <Input value={emoji} onChange={(e) => setEmoji(e.target.value)} maxLength={16} className="w-16 text-center" placeholder="🙂" />
          </Field>
          <div className="flex-1">
            <Field label={t("people.status.text")}>
              <Input value={text} onChange={(e) => setText(e.target.value)} maxLength={100} placeholder={t("people.status.placeholder")} />
            </Field>
          </div>
        </div>
        <Field label={t("people.status.clearAfter")}>
          <Select value={clear} onChange={(e) => setClear(e.target.value as ClearAfter)}>
            {card.status_until && <option value="keep">{t("people.status.clear.keep", { until: formatDate(card.status_until, i18n.language) })}</option>}
            {CLEAR_AFTER.map((c) => (
              <option key={c} value={c}>
                {t(`people.status.clear.${c}`)}
              </option>
            ))}
          </Select>
        </Field>
        {save.error && <p className="text-sm text-bad">{save.error.message}</p>}
        <div className="flex flex-wrap justify-end gap-2">
          {(card.status_text || card.status_emoji) && (
            <Button
              type="button"
              variant="ghost"
              disabled={save.isPending}
              onClick={() => save.mutate({ presence: card.presence, status_emoji: null, status_text: null, status_until: null }, { onSuccess: onClose })}
            >
              {t("people.status.clearNow")}
            </Button>
          )}
          <Button type="submit" variant="primary" disabled={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
