declare const el: HTMLElement & { setHTMLUnsafe(h: string): void };
declare const s: string;
export {};
el.setHTMLUnsafe(s);
