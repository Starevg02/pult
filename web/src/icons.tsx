// Значки интерфейса — встроенный SVG (без внешних шрифтов и пакетов: CSP `default-src 'self'`).
// Контур 24×24, линия 1.75, цвет — `currentColor`. Значок рядом с подписью скрыт от экранного диктора;
// у кнопки только со значком подпись даёт `aria-label`.
import type { ReactNode, SVGProps } from "react";

type IconProps = Omit<SVGProps<SVGSVGElement>, "children"> & { size?: number };

function make(name: string, body: ReactNode) {
  function Icon({ size = 16, className = "", ...p }: IconProps) {
    return (
      <svg
        xmlns="http://www.w3.org/2000/svg"
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.75}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
        focusable="false"
        className={`shrink-0 ${className}`}
        {...p}
      >
        {body}
      </svg>
    );
  }
  Icon.displayName = `Icon${name}`;
  return Icon;
}

export const IconHome = make("Home", <><path d="M3 10.5 12 3l9 7.5" /><path d="M5 9.5V20h5v-6h4v6h5V9.5" /></>);
export const IconPlus = make("Plus", <><path d="M12 5v14" /><path d="M5 12h14" /></>);
export const IconPlusSquare = make("PlusSquare", <><rect x="3.5" y="3.5" width="17" height="17" rx="4" /><path d="M12 8v8" /><path d="M8 12h8" /></>);
export const IconTicket = make("Ticket", <><path d="M3.5 8.5V6a1.5 1.5 0 0 1 1.5-1.5h14A1.5 1.5 0 0 1 20.5 6v2.5a2.5 2.5 0 0 0 0 5V18a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18v-4.5a2.5 2.5 0 0 0 0-5Z" /><path d="M14 4.5v2" /><path d="M14 10.5v3" /><path d="M14 17.5v2" /></>);
export const IconKey = make("Key", <><circle cx="8" cy="15" r="4" /><path d="m10.8 12.2 8.7-8.7" /><path d="m17 6 2.5 2.5" /><path d="m14.5 8.5 2 2" /></>);
export const IconCheckCircle = make("CheckCircle", <><circle cx="12" cy="12" r="8.5" /><path d="m8.5 12 2.5 2.5 4.5-5" /></>);
export const IconInbox = make("Inbox", <><path d="M3.5 13.5 6 5.5A1.5 1.5 0 0 1 7.4 4.5h9.2A1.5 1.5 0 0 1 18 5.5l2.5 8" /><path d="M3.5 13.5V18a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-4.5h-5l-1.5 2.5h-4l-1.5-2.5Z" /></>);
export const IconBook = make("Book", <><path d="M4.5 5.5A1.5 1.5 0 0 1 6 4h13.5v14H6a1.5 1.5 0 0 0-1.5 1.5Z" /><path d="M4.5 19.5A1.5 1.5 0 0 0 6 21h13.5v-3" /><path d="M9 8.5h6" /></>);
export const IconBox = make("Box", <><path d="m12 3 8.5 4.5v9L12 21l-8.5-4.5v-9Z" /><path d="m3.5 7.5 8.5 4.5 8.5-4.5" /><path d="M12 12v9" /></>);
export const IconMonitor = make("Monitor", <><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8.5 20h7" /><path d="M12 16v4" /></>);
export const IconChart = make("Chart", <><path d="M4 20V4" /><path d="M4 20h16" /><path d="M8.5 16v-5" /><path d="M13 16V8" /><path d="M17.5 16v-3" /></>);
export const IconContacts = make("Contacts", <><rect x="4" y="3" width="16" height="18" rx="2" /><circle cx="12" cy="10" r="2.75" /><path d="M7.5 17a4.5 4.5 0 0 1 9 0" /></>);
export const IconSettings = make("Settings", <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" /></>);
export const IconSearch = make("Search", <><circle cx="11" cy="11" r="6.5" /><path d="m20 20-4.4-4.4" /></>);
export const IconBell = make("Bell", <><path d="M6 9a6 6 0 0 1 12 0c0 6 2.5 7.5 2.5 7.5h-17S6 15 6 9Z" /><path d="M10.3 20a2 2 0 0 0 3.4 0" /></>);
export const IconSun = make("Sun", <><circle cx="12" cy="12" r="4" /><path d="M12 2.5v2" /><path d="M12 19.5v2" /><path d="m5.3 5.3 1.4 1.4" /><path d="m17.3 17.3 1.4 1.4" /><path d="M2.5 12h2" /><path d="M19.5 12h2" /><path d="m5.3 18.7 1.4-1.4" /><path d="m17.3 6.7 1.4-1.4" /></>);
export const IconMoon = make("Moon", <path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z" />);
export const IconMenu = make("Menu", <><path d="M4 6.5h16" /><path d="M4 12h16" /><path d="M4 17.5h16" /></>);
export const IconChevronDown = make("ChevronDown", <path d="m6 9 6 6 6-6" />);
export const IconChevronRight = make("ChevronRight", <path d="m9 6 6 6-6 6" />);
export const IconArrowLeft = make("ArrowLeft", <><path d="M19 12H5" /><path d="m11 6-6 6 6 6" /></>);
export const IconArrowRight = make("ArrowRight", <><path d="M5 12h14" /><path d="m13 6 6 6-6 6" /></>);
export const IconX = make("X", <><path d="M6 6l12 12" /><path d="M18 6 6 18" /></>);
export const IconMore = make("More", <><circle cx="5.5" cy="12" r="1.25" fill="currentColor" stroke="none" /><circle cx="12" cy="12" r="1.25" fill="currentColor" stroke="none" /><circle cx="18.5" cy="12" r="1.25" fill="currentColor" stroke="none" /></>);
export const IconFilter = make("Filter", <path d="M4 5h16l-6.5 7.5V19l-3 1.5v-8Z" />);
export const IconSort = make("Sort", <><path d="M7 4v16" /><path d="m3.5 16.5 3.5 3.5 3.5-3.5" /><path d="M13.5 6h7" /><path d="M13.5 11h5" /><path d="M13.5 16h3" /></>);
export const IconHeart = make("Heart", <path d="M12 20s-7.5-4.6-7.5-10.1A4.4 4.4 0 0 1 12 7.1a4.4 4.4 0 0 1 7.5 2.8C19.5 15.4 12 20 12 20Z" />);
export const IconTrash = make("Trash", <><path d="M4 7h16" /><path d="M9.5 7V4.5h5V7" /><path d="M6 7l1 12.5h10L18 7" /></>);
export const IconCamera = make("Camera", <><path d="M4 8h3l1.5-2.5h7L17 8h3v11H4Z" /><circle cx="12" cy="13" r="3.5" /></>);
export const IconPaperclip = make("Paperclip", <path d="m20 11.5-7.8 7.8a5 5 0 0 1-7.1-7.1l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4l7.8-7.8" />);
export const IconStar = make("Star", <path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9Z" />);
export const IconCopy = make("Copy", <><rect x="8.5" y="8.5" width="12" height="12" rx="2" /><path d="M15.5 8.5V5.5a2 2 0 0 0-2-2h-8a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h3" /></>);
export const IconCheck = make("Check", <path d="m5 12.5 4.5 4.5L19 7.5" />);
export const IconPhone = make("Phone", <path d="M5 3.5h3.5l1.8 4.5-2.3 1.4a11 11 0 0 0 6.6 6.6l1.4-2.3 4.5 1.8V19a1.5 1.5 0 0 1-1.6 1.5A16.5 16.5 0 0 1 3.5 5.1 1.5 1.5 0 0 1 5 3.5Z" />);
export const IconMail = make("Mail", <><rect x="3" y="5" width="18" height="14" rx="2" /><path d="m3.5 6.5 8.5 6.5 8.5-6.5" /></>);
export const IconColumns = make("Columns", <><rect x="3.5" y="4" width="17" height="16" rx="2" /><path d="M9.5 4v16" /><path d="M14.5 4v16" /></>);
export const IconArchive = make("Archive", <><rect x="3" y="4" width="18" height="4.5" rx="1" /><path d="M5 8.5V19a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8.5" /><path d="M10 12.5h4" /></>);
export const IconKeyboard = make("Keyboard", <><rect x="2.5" y="6" width="19" height="12" rx="2" /><path d="M6.5 10h.01" /><path d="M10 10h.01" /><path d="M14 10h.01" /><path d="M17.5 10h.01" /><path d="M7.5 14h9" /></>);
export const IconUser = make("User", <><circle cx="12" cy="8" r="4" /><path d="M4.5 20.5a7.5 7.5 0 0 1 15 0" /></>);
export const IconUserCheck = make("UserCheck", <><circle cx="9.5" cy="8" r="3.75" /><path d="M3 20a6.5 6.5 0 0 1 13 0" /><path d="m16 11 2 2 3.5-4" /></>);
export const IconTransfer = make("Transfer", <><path d="M4 8h14" /><path d="m14.5 4.5 3.5 3.5-3.5 3.5" /><path d="M20 16H6" /><path d="m9.5 12.5-3.5 3.5 3.5 3.5" /></>);
export const IconHelp = make("Help", <><circle cx="12" cy="12" r="8.5" /><path d="M9.6 9.5a2.5 2.5 0 0 1 4.8.8c0 1.7-2.4 2.2-2.4 3.7" /><path d="M12 17h.01" /></>);
export const IconPause = make("Pause", <><circle cx="12" cy="12" r="8.5" /><path d="M10 9v6" /><path d="M14 9v6" /></>);
export const IconMerge = make("Merge", <><circle cx="6.5" cy="5.5" r="2" /><circle cx="6.5" cy="18.5" r="2" /><circle cx="17.5" cy="12" r="2" /><path d="M6.5 7.5v9" /><path d="M6.5 8.5c0 3.5 3 3.5 9 3.5" /></>);
export const IconFileText = make("FileText", <><path d="M14 3.5H6.5a1.5 1.5 0 0 0-1.5 1.5v14a1.5 1.5 0 0 0 1.5 1.5h11A1.5 1.5 0 0 0 19 19V8.5Z" /><path d="M14 3.5v5h5" /><path d="M8.5 13h7" /><path d="M8.5 16.5h5" /></>);
export const IconMegaphone = make("Megaphone", <><path d="M4 10v4a1 1 0 0 0 1 1h2.5L15 19V5L7.5 9H5a1 1 0 0 0-1 1Z" /><path d="M18.5 9a4 4 0 0 1 0 6" /><path d="M8 15l1.5 5" /></>);
export const IconChat = make("Chat", <><path d="M4.5 5.5h15a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H10l-4.5 3.5v-3.5h-1a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1Z" /><path d="M8 10h8" /><path d="M8 13h5" /></>);
export const IconLink = make("Link", <><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" /><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" /></>);
export const IconShield = make("Shield", <><path d="M12 3.5 19.5 6v5.5c0 4.5-3.2 7.8-7.5 9-4.3-1.2-7.5-4.5-7.5-9V6Z" /><path d="m9 12 2 2 4-4" /></>);
export const IconBan = make("Ban", <><circle cx="12" cy="12" r="8.5" /><path d="m6 6 12 12" /></>);
export const IconRotate = make("Rotate", <><path d="M4 12a8 8 0 0 1 13.7-5.7L20 8.5" /><path d="M20 4v4.5h-4.5" /><path d="M20 12a8 8 0 0 1-13.7 5.7L4 15.5" /><path d="M4 20v-4.5h4.5" /></>);
export const IconLogout = make("Logout", <><path d="M9.5 20.5H6a1.5 1.5 0 0 1-1.5-1.5V5A1.5 1.5 0 0 1 6 3.5h3.5" /><path d="M15.5 16.5 20 12l-4.5-4.5" /><path d="M20 12H9.5" /></>);
export const IconAlert = make("Alert", <><path d="M10.3 4.2 2.8 17.5A2 2 0 0 0 4.5 20.5h15a2 2 0 0 0 1.7-3L13.7 4.2a2 2 0 0 0-3.4 0Z" /><path d="M12 9.5v4" /><path d="M12 17h.01" /></>);
export const IconInfo = make("Info", <><circle cx="12" cy="12" r="8.5" /><path d="M12 11v5" /><path d="M12 8h.01" /></>);
export const IconClock = make("Clock", <><circle cx="12" cy="12" r="8.5" /><path d="M12 7.5V12l3 2" /></>);
export const IconSparkles = make("Sparkles", <><path d="M12 3.5 13.8 9l5.7 1.8-5.7 1.8L12 18.5l-1.8-5.9L4.5 10.8 10.2 9Z" /><path d="M19 3v3" /><path d="M17.5 4.5h3" /></>);
export const IconBellOff = make("BellOff", <><path d="M8.7 3.9A6 6 0 0 1 18 9c0 3 .6 4.9 1.3 6" /><path d="M6.2 6.6A6 6 0 0 0 6 9c0 6-2.5 7.5-2.5 7.5h13" /><path d="M10.3 20a2 2 0 0 0 3.4 0" /><path d="m3 3 18 18" /></>);
export const IconSend = make("Send", <><path d="M21 3 10 14" /><path d="M21 3 14.5 21l-4.5-7-7-4.5Z" /></>);
export const IconSmile = make("Smile", <><circle cx="12" cy="12" r="9" /><path d="M8.5 14.5a4.5 4.5 0 0 0 7 0" /><path d="M9 9.5h.01" /><path d="M15 9.5h.01" /></>);
export const IconReply = make("Reply", <><path d="M9.5 7 4.5 12l5 5" /><path d="M4.5 12h9a6 6 0 0 1 6 6v1" /></>);
export const IconPin = make("Pin", <><path d="M12 16.5V21" /><path d="M8 3.5h8l-1 6 3 3.5v1.5H6V13l3-3.5Z" /></>);
export const IconCheckDouble = make("CheckDouble", <><path d="m2.5 12.5 4.5 4.5L16.5 7.5" /><path d="m11.5 16 1 1L22 7.5" /></>);
export const IconChevronUp = make("ChevronUp", <path d="m6 15 6-6 6 6" />);
