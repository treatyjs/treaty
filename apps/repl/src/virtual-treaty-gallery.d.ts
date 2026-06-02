/** Ambient type for the build-time Ivy gallery (see tools/gallery-plugin.ts). */
declare module 'virtual:treaty-gallery' {
	export interface GalleryRow {
		readonly id: string
		readonly label: string
		readonly kind: 'treaty' | 'jsx' | 'component'
		readonly plugin: string
		/** The original authoring source. */
		readonly source: string
		/** The emitted Ivy JavaScript. */
		readonly code: string
		readonly serverModule?: string
		readonly sideEffects: boolean
		readonly error?: string
		readonly owned: boolean
	}
	const rows: readonly GalleryRow[]
	export default rows
}
