// База знаний: разметка статьи (оглавление, время чтения) и правка markdown кнопками панели.
import type { ReactNode } from "react";
import { Children, isValidElement } from "react";

/** Текст из детей узла markdown — для якорей заголовков. */
export function textOf(children: ReactNode): string {
  let out = "";
  Children.forEach(children, (c) => {
    if (typeof c === "string" || typeof c === "number") out += String(c);
    else if (isValidElement<{ children?: ReactNode }>(c)) out += textOf(c.props.children);
  });
  return out;
}

/** Якорь заголовка: буквы любого алфавита, цифры и дефисы. */
export function slug(text: string): string {
  return (
    text
      .toLowerCase()
      .trim()
      .replace(/[^\p{L}\p{N}\s-]/gu, "")
      .replace(/\s+/g, "-")
      .slice(0, 80) || "section"
  );
}

/** `line` — номер строки в тексте (с единицы), как в позициях разбора markdown. */
export type Heading = { level: 2 | 3; text: string; id: string; line: number };

/** Заголовки статьи для оглавления (`#`/`##` — второй уровень, `###` и мельче — третий). Код пропускается. */
export function headings(md: string): Heading[] {
  const out: Heading[] = [];
  const seen = new Map<string, number>();
  let code = false;
  for (const [i, line] of md.split("\n").entries()) {
    if (line.trimStart().startsWith("```")) {
      code = !code;
      continue;
    }
    const m = !code && /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (!m) continue;
    const text = (m[2] ?? "").replace(/[*_`~]/g, "");
    out.push({ level: (m[1] ?? "").length <= 2 ? 2 : 3, text, id: uniqueId(seen, slug(text)), line: i + 1 });
  }
  return out;
}

/** Одинаковые заголовки получают «-2», «-3» — как и при показе статьи. */
export function uniqueId(seen: Map<string, number>, id: string): string {
  const n = (seen.get(id) ?? 0) + 1;
  seen.set(id, n);
  return n === 1 ? id : `${id}-${n}`;
}

/** Минут на чтение (≈ 180 слов в минуту), не меньше одной. */
export function readingMinutes(md: string): number {
  const words = md.replace(/!\[[^\]]*\]\([^)]*\)/g, " ").split(/\s+/).filter(Boolean).length;
  return Math.max(1, Math.round(words / 180));
}

/** Подпись картинки — это имя файла («image.png»): тогда подписи нет. */
export const looksLikeFileName = (s: string) => /\.[a-z0-9]{2,5}$/i.test(s.trim());

export type Edit = { value: string; start: number; end: number };

/** Обернуть выделение (`**`, `_`, `` ` ``); без выделения — вставить пример и выделить его. */
export function wrap(e: Edit, left: string, right: string, sample: string): Edit {
  const sel = e.value.slice(e.start, e.end) || sample;
  const value = e.value.slice(0, e.start) + left + sel + right + e.value.slice(e.end);
  return { value, start: e.start + left.length, end: e.start + left.length + sel.length };
}

/** Префикс строк выделения (`## `, `> `, `- `, `1. `); повторное нажатие снимает его. */
export function prefixLines(e: Edit, prefix: string | ((i: number) => string)): Edit {
  const from = e.value.lastIndexOf("\n", e.start - 1) + 1;
  const toNl = e.value.indexOf("\n", e.end);
  const to = toNl < 0 ? e.value.length : toNl;
  const lines = e.value.slice(from, to).split("\n");
  const p = (i: number) => (typeof prefix === "string" ? prefix : prefix(i));
  const all = lines.every((l, i) => l.startsWith(p(i)));
  // Другой значок заменяется: из заголовка — в цитату, из списка — в нумерованный.
  const strip = (l: string) => l.replace(/^(#{1,6} |> |- |\d+\. )/, "");
  const next = lines.map((l, i) => (all ? l.slice(p(i).length) : l.trim() === "" && lines.length > 1 ? l : p(i) + strip(l))).join("\n");
  const value = e.value.slice(0, from) + next + e.value.slice(to);
  return { value, start: from, end: from + next.length };
}

/** Вставить блок отдельными строками (картинку, разделитель, код). */
export function insertBlock(e: Edit, block: string): Edit {
  const before = e.value.slice(0, e.start);
  const after = e.value.slice(e.end);
  const lead = before === "" || before.endsWith("\n\n") ? "" : before.endsWith("\n") ? "\n" : "\n\n";
  const tail = after.startsWith("\n") ? "" : "\n";
  const value = before + lead + block + tail + after;
  const pos = before.length + lead.length + block.length;
  return { value, start: pos, end: pos };
}

/** Ссылка: выделенный текст становится подписью. */
export function link(e: Edit, url: string, sample: string): Edit {
  const text = e.value.slice(e.start, e.end) || sample;
  const md = `[${text}](${url})`;
  const value = e.value.slice(0, e.start) + md + e.value.slice(e.end);
  return { value, start: e.start + 1, end: e.start + 1 + text.length };
}
