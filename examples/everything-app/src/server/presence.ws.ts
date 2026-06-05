/**
 * WebSocket-transport server functions.
 *
 * The `'use websocket'` prologue tells Treaty to lower the exported functions
 * onto a duplex WebSocket channel instead of a one-shot request. A `ws`-prefixed
 * export name is the conventional marker for an individual socket handler. The
 * client binding the compiler emits is a long-lived subscription, not a fetch.
 *
 * Transport: WebSocket (bidirectional, push).
 */
'use websocket'

export interface PresenceEvent {
	readonly userId: string
	readonly status: 'online' | 'offline'
	readonly at: number
}

export interface PresenceSocket {
	/** Push a presence update to every other connected peer. */
	announce(status: 'online' | 'offline'): void
	/** Close the channel for this peer. */
	close(): void
}

/**
 * Open a presence channel. `onEvent` fires for every peer presence change for
 * as long as the socket is open. The body runs on the server; the compiler
 * gives the client a `PresenceSocket` handle bound to the live connection.
 */
export function wsPresence(
	userId: string,
	onEvent: (event: PresenceEvent) => void
): PresenceSocket {
	// Server-side: register this peer, fan messages out to others. The body is
	// extracted to the server module and never shipped to the browser.
	const broadcast = (status: 'online' | 'offline'): void => {
		onEvent({ userId, status, at: Date.now() })
	}
	broadcast('online')
	return {
		announce: (status) => broadcast(status),
		close: () => broadcast('offline'),
	}
}
