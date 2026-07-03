/**
 * REST client for the scanner admin API. All calls are same-origin admin
 * requests carrying the WP nonce header.
 */

interface ApiOptions {
    method: string;
    body?: string;
}

declare const feedScannerConfig: { restBase: string; nonce: string };

async function request<T>(path: string, options: ApiOptions): Promise<T> {
    const response = await fetch(`${feedScannerConfig.restBase}${path}`, {
        method: options.method,
        headers: {
            'Content-Type': 'application/json',
            'X-WP-Nonce': feedScannerConfig.nonce,
        },
        body: options.body,
        credentials: 'same-origin',
    });
    if (!response.ok) {
        throw new Error(`API ${path} failed: ${response.status}`);
    }
    return (await response.json()) as T;
}

export function startScan(): Promise<{ started: boolean }> {
    return request('/scan/start', { method: 'POST' });
}

export function fetchProgress(): Promise<{ done: number; total: number; score: number | null }> {
    return request('/scan/progress', { method: 'GET' });
}

export function fetchReportUrl(): Promise<{ url: string }> {
    return request('/scan/report', { method: 'GET' });
}

export interface HistoryEntry {
    scannedAt: string;
    score: number;
    critical: number;
    warning: number;
}

export function fetchHistory(): Promise<HistoryEntry[]> {
    return request('/scan/history', { method: 'GET' });
}

export function resetQueue(): Promise<{ cleared: boolean }> {
    return request('/scan/reset', { method: 'POST' });
}

export function fetchIssues(page: number): Promise<{ rows: unknown[]; pages: number }> {
    return request(`/scan/issues?page=${page}`, { method: 'GET' });
}
