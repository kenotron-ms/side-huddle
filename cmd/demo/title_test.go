//go:build darwin

package main

import "testing"

func TestPickBestMeetingTitle(t *testing.T) {
	cases := []struct {
		name   string
		titles []string
		want   string
	}{
		{
			name: "chat tab vs real meeting — meeting wins",
			titles: []string{
				"Chat | Amanda Silver",
				"Omar (Microsoft) <> Stuart Brown + Chris Scott (Guidehouse) — OpenClaw / Agentic Architectures | Microsoft Teams",
			},
			want: "Omar (Microsoft) <> Stuart Brown + Chris Scott (Guidehouse) — OpenClaw / Agentic Architectures",
		},
		{
			name: "chat tab front-most should not be chosen if a meeting is present",
			titles: []string{
				"Chat | Amanda Silver",
				"9:00-9:30 Project Lobster Review | Microsoft Teams",
			},
			want: "Project Lobster Review",
		},
		{
			name: "bare person name (older Teams chat) loses to a meeting title",
			titles: []string{
				"Sumit Chauhan | Microsoft Teams",
				"Gartner Interaction - (Ref#20001495) - Project Lobster | Microsoft Teams",
			},
			want: "Gartner Interaction - (Ref#20001495) - Project Lobster",
		},
		{
			name: "only a chat tab → empty (we keep whatever title we already had)",
			titles: []string{"Chat | Amanda Silver"},
			want:   "",
		},
		{
			name: "calendar tab is dropped entirely",
			titles: []string{
				"Calendar | Microsoft Teams",
				"Internal- GE Aerospace - Microsoft AI Productivity Vision | Microsoft Teams",
			},
			want: "Internal- GE Aerospace - Microsoft AI Productivity Vision",
		},
		{
			name: "single non-chat window with a meeting-shaped title is used",
			titles: []string{"Project Lobster Review | Microsoft Teams"},
			want:   "Project Lobster Review",
		},
		{
			name: "person-name-only chat (older format) without a real meeting → still returns it as last resort",
			// We can't tell "John Smith" from a 1:1 meeting subject just from text,
			// so a low-score result is allowed when there's nothing else.
			titles: []string{"John Smith | Microsoft Teams"},
			want:   "John Smith",
		},
	}
	for _, tc := range cases {
		got := pickBestMeetingTitle(tc.titles, "Microsoft Teams")
		if got != tc.want {
			t.Errorf("%s:\n  titles = %v\n  got    = %q\n  want   = %q", tc.name, tc.titles, got, tc.want)
		}
	}
}
