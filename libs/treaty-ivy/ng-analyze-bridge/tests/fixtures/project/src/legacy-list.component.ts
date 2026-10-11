import { Component, Input, Output, EventEmitter, HostListener, ViewChildren, QueryList, ElementRef, ContentChild, TemplateRef, ViewEncapsulation } from '@angular/core';

@Component({
  selector: 'app-legacy-list',
  templateUrl: './legacy-list.component.html',
  styleUrls: ['./legacy-list.component.css'],
  encapsulation: ViewEncapsulation.None,
  providers: [],
})
export class LegacyListComponent {
  @Input({ required: true, alias: 'items' }) rows: string[] = [];
  @Input() title = 'list';
  @Output('picked') pick = new EventEmitter<string>();
  @ViewChildren('row') rowEls!: QueryList<ElementRef<HTMLLIElement>>;
  @ContentChild(TemplateRef) emptyTpl?: TemplateRef<unknown>;

  @HostListener('window:resize', ['$event.target'])
  onResize(target: unknown) { return target; }
}
