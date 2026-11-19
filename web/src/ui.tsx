// Общие элементы интерфейса в стиле макетов (mockups/v7): графитовые основные кнопки, индиго — акцент.
import { useEffect, useId, useRef, useState, type ComponentProps, type KeyboardEvent as ReactKeyboardEvent, type ReactNode, type Ref } from "react";
import { useTranslation } from "react-i18next";
import { ApiError } from "./api/client";
import { IconAlert, IconCheck, IconChevronDown, IconCopy, IconInbox, IconInfo, IconX } from "./icons";
import { dismissToast, pauseToasts, useToasts } from "./toast";

type Variant = "primary" | "secondary" | "ghost" | "danger";

const variants: Record<Variant, string> = {
  primary: "bg-ink text-paper shadow-sm hover:opacity-90",
  secondary: "border border-line-strong bg-paper text-ink shadow-xs hover:bg-hover",
  ghost: "text-muted hover:bg-hover hover:text-ink",
  danger: "border border-bad/40 bg-paper text-bad hover:bg-bad/5",
};

export function Button({ variant = "secondary", className = "", ...p }: ComponentProps<"button"> & { variant?: Variant }) {
  return (
    <button
      type="button"
      {...p}
      className={`inline-flex min-h-8 items-center justify-center gap-2 rounded-lg px-3 py-1.5 text-sm font-medium transition-[background-color,opacity,box-shadow] active:translate-y-px disabled:cursor-not-allowed disabled:opacity-50 ${variants[variant]} ${className}`}
    />
  );
}

/** Ширина по умолчанию — во всю строку; своя ширина (`w-…`, `flex-1`) в className её заменяет
 *  (Tailwind не снимает конфликт `w-full w-auto` сам — побеждал `w-full`). */
const width = (className: string) => (/(^|\s)(w-|flex-1|max-w-)/.test(className) ? "" : "w-full");

export function Input({ className = "", ...p }: ComponentProps<"input">) {
  return (
    <input
      {...p}
      className={`${width(className)} rounded-lg border border-control bg-paper px-3 py-2 text-sm text-ink transition-colors placeholder:text-muted focus:border-accent disabled:bg-sunken ${className}`}
    />
  );
}

export function Select({ className = "", ...p }: ComponentProps<"select">) {
  return (
    <select
      {...p}
      className={`${width(className)} rounded-lg border border-control bg-paper px-3 py-2 text-sm text-ink focus:border-accent disabled:bg-sunken ${className}`}
    />
  );
}

/** Компактный фильтр-«чип» для панелей инструментов: подпись — для экранного диктора, значение видно.
 *  `active` — фильтр отличается от значения по умолчанию (подсвечен). */
export function ChipSelect({ label, icon, active, className = "", ...p }: ComponentProps<"select"> & { label: string; icon?: ReactNode; active?: boolean }) {
  // Сам «чип» — родной <select> во всю площадь: щелчок в любом месте открывает список, рамка фокуса — общая.
  return (
    <span className={`relative inline-flex shrink-0 items-center ${active ? "text-accent-ink" : "text-ink"} ${className}`}>
      {icon && <span className="pointer-events-none absolute left-3 flex">{icon}</span>}
      <select
        {...p}
        aria-label={label}
        className={`h-8 cursor-pointer appearance-none rounded-full border pr-8 text-sm font-medium transition-colors ${icon ? "pl-8" : "pl-3"} ${
          active ? "border-accent bg-accentsoft" : "border-line-strong bg-paper hover:bg-hover"
        }`}
      />
      <IconChevronDown size={14} className="pointer-events-none absolute right-3 opacity-70" />
    </span>
  );
}

/** Подпись поля. `group` — для составных полей (выбор сотрудника с кнопками): `<label>` вокруг них
 *  отдавал бы клик по подписи первой кнопке («Изменить» сбрасывала выбор). */
export function Field({ label, hint, children, group }: { label: string; hint?: ReactNode; children: ReactNode; group?: boolean }) {
  const id = useId();
  if (group)
    return (
      <div role="group" aria-labelledby={id} className="block">
        <span id={id} className="mb-1 block text-sm font-medium text-ink">
          {label}
        </span>
        {children}
        {hint && <span className="mt-1 block text-xs text-muted">{hint}</span>}
      </div>
    );
  return (
    <label className="block">
      <span className="mb-1 block text-sm font-medium text-ink">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-xs text-muted">{hint}</span>}
    </label>
  );
}

export function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`rounded-xl border border-line bg-paper shadow-xs ${className}`}>{children}</div>;
}

export function PageHeader({ title, children, subtitle }: { title: string; children?: ReactNode; subtitle?: ReactNode }) {
  return (
    <div className="mb-5 flex flex-wrap items-center gap-3">
      <div className="min-w-0">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {subtitle && <p className="mt-0.5 text-sm text-muted">{subtitle}</p>}
      </div>
      <div className="ml-auto flex flex-wrap items-center gap-2">{children}</div>
    </div>
  );
}

export function Badge({ children, tone = "muted" }: { children: ReactNode; tone?: "muted" | "ok" | "bad" | "accent" }) {
  const tones = {
    muted: "bg-sunken text-muted",
    ok: "bg-ok/10 text-ok",
    bad: "bg-bad/10 text-bad",
    accent: "bg-accentsoft text-accent-ink",
  };
  return <span className={`inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs font-medium whitespace-nowrap ${tones[tone]}`}>{children}</span>;
}

/** Серая «заготовка» на месте данных, пока они грузятся. */
export function Skeleton({ className = "" }: { className?: string }) {
  return <span aria-hidden="true" className={`block animate-pulse rounded-md bg-sunken ${className}`} />;
}

const LINE_WIDTHS = ["w-11/12", "w-4/5", "w-2/3", "w-3/4", "w-1/2"];

/** Загрузка: заготовка вместо текста «Загрузка…» (сам текст — для экранного диктора).
 *  `list` — строки списка (заявки, сотрудники), иначе — строки текста. */
export function Spinner({ rows, variant = "lines" }: { rows?: number; variant?: "lines" | "list" }) {
  const { t } = useTranslation();
  const n = rows ?? (variant === "list" ? 6 : 3);
  return (
    <div role="status" className={variant === "list" ? "overflow-hidden rounded-xl border border-line bg-paper" : "space-y-2.5 py-4"}>
      <span className="sr-only">{t("common.loading")}</span>
      {Array.from({ length: n }, (_, i) =>
        variant === "list" ? (
          <div key={i} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
            <Skeleton className="h-3 w-12" />
            <Skeleton className={`h-3 ${LINE_WIDTHS[i % LINE_WIDTHS.length]} max-w-md`} />
            <Skeleton className="ml-auto h-5 w-16 rounded-full" />
          </div>
        ) : (
          <Skeleton key={i} className={`h-3.5 ${LINE_WIDTHS[i % LINE_WIDTHS.length]}`} />
        ),
      )}
    </div>
  );
}

/** Ошибка запроса понятным текстом (сервер присылает его сам). */
export function ErrorBox({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const { t } = useTranslation();
  const text = error instanceof ApiError ? error.message : t("common.error");
  return (
    <div role="alert" className="flex items-center gap-3 rounded-lg border border-bad/30 bg-bad/5 px-3 py-2 text-sm text-bad">
      <IconAlert />
      <span>{text}</span>
      {onRetry && (
        <Button variant="ghost" className="ml-auto" onClick={onRetry}>
          {t("common.retry")}
        </Button>
      )}
    </div>
  );
}

/** Пустой список: что случилось, подсказка, что сделать, и кнопка действия. */
export function Empty({ children, hint, icon, action }: { children: ReactNode; hint?: ReactNode; icon?: ReactNode; action?: ReactNode }) {
  return (
    <div className="animate-fade-in flex flex-col items-center px-4 py-10 text-center">
      <span aria-hidden="true" className="mb-3 flex h-11 w-11 items-center justify-center rounded-full bg-sunken text-muted">
        {icon ?? <IconInbox size={22} />}
      </span>
      <div className="text-sm font-medium text-ink">{children}</div>
      {hint && <p className="mt-1 max-w-sm text-sm text-muted">{hint}</p>}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}

/** Модальное окно на `<dialog>`: Esc закрывает, фокус остаётся внутри. */
export function Dialog({ open, onClose, title, children, wide }: { open: boolean; onClose: () => void; title: string; children: ReactNode; wide?: boolean }) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  // Кто открыл окно — туда фокус и вернётся после закрытия (клавиатура не теряет место).
  const opener = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      d.showModal();
    }
    if (!open && d.open) d.close();
    if (!open && opener.current) {
      const back = opener.current;
      opener.current = null;
      if (back.isConnected) back.focus();
    }
  }, [open]);
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby={titleId}
      className={`pult-dialog m-auto w-[calc(100%-2rem)] rounded-2xl border border-line bg-paper p-0 text-ink shadow-2xl backdrop:bg-black/40 backdrop:backdrop-blur-[2px] ${wide ? "max-w-3xl" : "max-w-lg"}`}
    >
      {open && (
        <div className="relative p-5">
          <h2 id={titleId} className="mb-4 pr-8 text-base font-semibold">
            {title}
          </h2>
          {children}
          {/* Крестик — последним в порядке Tab: при открытии фокус получает первое поле окна. */}
          <CloseButton onClick={onClose} className="absolute top-3 right-3" />
        </div>
      )}
    </dialog>
  );
}

/** Кнопка-крестик «Закрыть» (24×24 и больше — WCAG 2.5.8). */
export function CloseButton({ onClick, className = "", label, ref }: { onClick: () => void; className?: string; label?: string; ref?: Ref<HTMLButtonElement> }) {
  const { t } = useTranslation();
  const text = label ?? t("common.close");
  return (
    <button ref={ref} type="button" onClick={onClick} aria-label={text} title={text} className={`inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted hover:bg-hover hover:text-ink ${className}`}>
      <IconX />
    </button>
  );
}

/** Скопировать значение в буфер; подтверждение рядом на пару секунд. */
export function CopyButton({ value, label }: { value: string; label: string }) {
  const { t } = useTranslation();
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      onClick={() => {
        void navigator.clipboard?.writeText(value).then(() => {
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        });
      }}
      className={`inline-flex min-h-6 min-w-6 items-center justify-center rounded px-1 text-xs hover:bg-hover hover:text-ink ${done ? "text-ok" : "text-muted"}`}
    >
      {done ? <IconCheck size={14} /> : <IconCopy size={14} />}
      {/* Экранному диктору — что скопировано. */}
      <span role="status" className="sr-only">
        {done ? t("common.copied") : ""}
      </span>
    </button>
  );
}

/** Всплывающие сообщения в углу экрана; область объявляет их экранному диктору. */
export function Toaster() {
  const { t } = useTranslation();
  const items = useToasts();
  const last = items[items.length - 1];
  return (
    <div
      role="region"
      aria-label={t("toast.region")}
      // Пока указатель или фокус на сообщениях — они не исчезают (WCAG 2.2.1).
      onMouseEnter={() => pauseToasts(true)}
      onMouseLeave={() => pauseToasts(false)}
      onFocus={() => pauseToasts(true)}
      onBlur={(e) => !e.currentTarget.contains(e.relatedTarget as Node) && pauseToasts(false)}
      className="pointer-events-none fixed inset-x-0 bottom-4 z-50 flex flex-col items-center gap-2 px-4 sm:inset-x-auto sm:right-4 sm:items-end"
    >
      {/* Диктору — только текст последнего сообщения (без кнопок «Закрыть»); область есть всегда, и пустая. */}
      <div role="status" className="sr-only">
        {last?.text ?? ""}
      </div>
      <div className="flex flex-col items-center gap-2 sm:items-end">
        {items.map((x) => (
          <div
            key={x.id}
            className="animate-toast-in pointer-events-auto flex max-w-sm items-center gap-2.5 rounded-xl border border-line bg-paper py-2.5 pr-2 pl-3.5 text-sm text-ink shadow-lg"
          >
            <span aria-hidden="true" className={x.tone === "ok" ? "text-ok" : x.tone === "bad" ? "text-bad" : "text-accent"}>
              {x.tone === "ok" ? <IconCheck /> : x.tone === "bad" ? <IconAlert /> : <IconInfo />}
            </span>
            <span className="min-w-0 flex-1">{x.text}</span>
            <CloseButton onClick={() => dismissToast(x.id)} className="h-7 w-7" />
          </div>
        ))}
      </div>
    </div>
  );
}

export type MenuEntry = { key: string; label: string; icon?: ReactNode; onSelect: () => void; danger?: boolean; disabled?: boolean; separated?: boolean };

/** Кнопка с выпадающим меню (WAI-ARIA menu button): стрелки, Home/End, первая буква, Esc — назад на кнопку. */
export function MenuButton({ label, icon, items, variant = "secondary", iconOnly }: { label: string; icon?: ReactNode; items: MenuEntry[]; variant?: Variant; iconOnly?: boolean }) {
  const [open, setOpen] = useState(false);
  const [alignEnd, setAlignEnd] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const menuId = useId();
  const triggerId = useId();
  const enabled = items.map((x, i) => (x.disabled ? -1 : i)).filter((i) => i >= 0);
  const focusAt = (i: number | undefined) => i !== undefined && refs.current[i]?.focus();
  const show = (first: "first" | "last") => {
    const r = trigger.current?.getBoundingClientRect();
    // У правого края экрана меню раскрывается влево, чтобы не уйти за край (телефон).
    setAlignEnd(!!r && r.left + 260 > window.innerWidth);
    setOpen(true);
    requestAnimationFrame(() => focusAt(first === "first" ? enabled[0] : enabled[enabled.length - 1]));
  };
  const hide = (refocus: boolean) => {
    setOpen(false);
    if (refocus) trigger.current?.focus();
  };
  useEffect(() => {
    if (!open) return;
    const outside = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", outside);
    return () => document.removeEventListener("mousedown", outside);
  }, [open]);
  const onMenuKey = (e: ReactKeyboardEvent) => {
    const cur = refs.current.findIndex((el) => el === document.activeElement);
    const pos = enabled.indexOf(cur);
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (enabled.length === 0) return;
      const down = e.key === "ArrowDown";
      // Фокус не на пункте — к первому (вниз) или последнему (вверх).
      const next = pos < 0 ? (down ? 0 : enabled.length - 1) : (pos + (down ? 1 : -1) + enabled.length) % enabled.length;
      focusAt(enabled[next]);
    } else if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      focusAt(e.key === "Home" ? enabled[0] : enabled[enabled.length - 1]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      hide(true);
    } else if (e.key === "Tab") {
      // Фокус — на кнопку меню до закрытия: Tab продолжит от неё, а не с начала страницы.
      trigger.current?.focus();
      setOpen(false);
    } else if (e.key.length === 1 && /\S/.test(e.key) && !e.ctrlKey && !e.metaKey && !e.altKey) {
      // Переход к пункту на эту букву (после текущего, по кругу); горячие клавиши страницы буква не трогает.
      e.preventDefault();
      const k = e.key.toLowerCase();
      const order = [...enabled.slice(pos + 1), ...enabled.slice(0, pos + 1)];
      focusAt(order.find((i) => items[i]?.label.toLowerCase().startsWith(k)));
    }
  };
  return (
    <div ref={box} className="relative inline-block">
      <Button
        ref={trigger}
        id={triggerId}
        variant={variant}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        aria-label={iconOnly ? label : undefined}
        title={iconOnly ? label : undefined}
        className={iconOnly ? "w-8 px-0" : ""}
        onClick={() => (open ? hide(false) : show("first"))}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            show(e.key === "ArrowDown" ? "first" : "last");
          }
        }}
      >
        {icon}
        {!iconOnly && label}
        {!iconOnly && <IconChevronDown size={14} className={`transition-transform ${open ? "rotate-180" : ""}`} />}
      </Button>
      {open && (
        <div
          id={menuId}
          role="menu"
          aria-labelledby={triggerId}
          onKeyDown={onMenuKey}
          className={`animate-pop-in absolute top-full z-40 mt-1.5 w-max max-w-[calc(100vw-2rem)] min-w-56 rounded-xl border border-line bg-paper p-1 shadow-xl ${alignEnd ? "right-0 origin-top-right" : "left-0 origin-top-left"}`}
        >
          {items.map((x, i) => (
            <div key={x.key}>
              {x.separated && i > 0 && <div role="separator" className="my-1 border-t border-line" />}
              <button
                ref={(el) => {
                  refs.current[i] = el;
                }}
                type="button"
                role="menuitem"
                tabIndex={-1}
                disabled={x.disabled}
                onClick={() => {
                  // Фокус — на кнопку меню: окно, которое откроет пункт, вернёт его туда.
                  hide(true);
                  x.onSelect();
                }}
                className={`flex min-h-9 w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-sm outline-none hover:bg-hover focus:bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent disabled:opacity-50 ${x.danger ? "text-bad" : "text-ink"}`}
              >
                <span aria-hidden="true" className={x.danger ? "text-bad" : "text-muted"}>
                  {x.icon}
                </span>
                {x.label}
              </button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
