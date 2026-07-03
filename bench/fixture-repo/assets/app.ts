/**
 * Dashboard frontend: polls scan progress and updates the score card.
 */

import { fetchProgress, startScan } from './api';

interface ScanProgress {
    done: number;
    total: number;
    score: number | null;
}

const POLL_INTERVAL_MS = 4000;

export function initDashboard(rootEl: HTMLElement): void {
    const button = rootEl.querySelector<HTMLButtonElement>('.js-start-scan');
    if (button) {
        button.addEventListener('click', onStartScan);
    }
    void pollLoop(rootEl);
}

async function onStartScan(event: Event): Promise<void> {
    event.preventDefault();
    const target = event.currentTarget as HTMLButtonElement;
    target.disabled = true;
    try {
        await startScan();
    } finally {
        target.disabled = false;
    }
}

async function pollLoop(rootEl: HTMLElement): Promise<void> {
    const bar = rootEl.querySelector<HTMLElement>('.js-progress');
    for (;;) {
        const progress: ScanProgress = await fetchProgress();
        if (bar) {
            renderProgress(bar, progress);
        }
        if (progress.total > 0 && progress.done >= progress.total) {
            break;
        }
        await sleep(POLL_INTERVAL_MS);
    }
}

function renderProgress(el: HTMLElement, progress: ScanProgress): void {
    const pct = progress.total === 0 ? 0 : Math.round((progress.done / progress.total) * 100);
    el.style.width = `${pct}%`;
    el.setAttribute('aria-valuenow', String(pct));
}

function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Issue table interactions: client side filtering and sorting for the
 * rendered issue list, no server round trips.
 */

type Level = 'critical' | 'warning';

interface IssueRow {
    productId: number;
    rule: string;
    level: Level;
    message: string;
}

export function initIssueTable(tableEl: HTMLTableElement): void {
    const filterInput = document.querySelector<HTMLInputElement>('.js-issue-filter');
    if (filterInput) {
        filterInput.addEventListener('input', () => {
            applyFilter(tableEl, filterInput.value.trim().toLowerCase());
        });
    }
    for (const header of tableEl.querySelectorAll<HTMLTableCellElement>('th[data-sort]')) {
        header.addEventListener('click', () => sortBy(tableEl, header.dataset.sort ?? ''));
    }
}

function applyFilter(tableEl: HTMLTableElement, needle: string): void {
    for (const row of tableEl.querySelectorAll<HTMLTableRowElement>('tbody tr')) {
        const text = row.textContent?.toLowerCase() ?? '';
        row.hidden = needle.length > 0 && !text.includes(needle);
    }
}

function sortBy(tableEl: HTMLTableElement, column: string): void {
    const body = tableEl.tBodies[0];
    const rows = Array.from(body.rows);
    const index = Number(column);
    rows.sort((a, b) => {
        const av = a.cells[index]?.textContent ?? '';
        const bv = b.cells[index]?.textContent ?? '';
        return av.localeCompare(bv, undefined, { numeric: true });
    });
    for (const row of rows) {
        body.appendChild(row);
    }
}
