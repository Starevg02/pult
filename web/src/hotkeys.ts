// Горячие клавиши страниц.

/** Горячую клавишу страницы пропускаем: при вводе текста, в открытом диалоге, с модификаторами, а Enter и
 *  пробел — ещё и на кнопке или ссылке (там это их собственное действие, иначе сработали бы оба). */
export function ignoreHotkey(e: KeyboardEvent): boolean {
  const el = e.target instanceof Element ? e.target : null;
  if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return true;
  if (el?.closest("input,textarea,select,[contenteditable=true],dialog,[role=menu]") || document.querySelector("dialog[open]")) return true;
  return (e.key === "Enter" || e.key === " ") && !!el?.closest("a,button,summary,[role=button],[role=link],label");
}
