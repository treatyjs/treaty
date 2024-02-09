import { ɵprovideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { beforeEach, describe, expect, test } from 'bun:test';
import PostComponent from './post.component';

describe('PostComponent', () => {
  let component: PostComponent;
  let fixture: ComponentFixture<PostComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [PostComponent],
      providers: [provideRouter([]), ɵprovideZonelessChangeDetection()],
    }).compileComponents();

    fixture = TestBed.createComponent(PostComponent);
    fixture.componentRef.setInput('id', 'test');
    component = fixture.componentInstance;
    await fixture.whenStable();
  });

  test('should create the app', () => {
    expect(component).toBeTruthy();
  });

  test(`should have as title 'treaty'`, () => {
    expect(component.title).toEqual('treaty');
  });
});
