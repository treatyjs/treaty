// Angular @Pipe (.ts) — standalone pipe with PipeTransform demonstrating text truncation.
// Shows idiomatic pipe implementation with optional parameters for length and ellipsis.
import { Pipe, type PipeTransform } from '@angular/core'

@Pipe({ 
	name: 'truncate',
	standalone: true,
})
export class TruncatePipe implements PipeTransform {
	transform(value: string, length: number = 50, suffix: string = '...'): string {
		if (!value) {
			return value
		}
		
		if (value.length <= length) {
			return value
		}
		
		const truncated = value.substring(0, length).trim()
		return truncated + suffix
	}
}
