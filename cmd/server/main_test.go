package main

import (
	"reflect"
	"testing"
)

func TestParseRoles(t *testing.T) {
	tests := []struct {
		name    string
		raw     string
		wantAll bool
		want    []string // ignored when wantAll
	}{
		{"unset", "", true, nil},
		{"blank", "   ", true, nil},
		{"only commas", " , ,", true, nil},
		{"single", "accounts", false, []string{"accounts"}},
		{"multi", "accounts,characters", false, []string{"accounts", "characters"}},
		{"spaces and dupes", " accounts , characters, accounts ", false, []string{"accounts", "characters"}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			rs := parseRoles(tc.raw)
			if rs.all != tc.wantAll {
				t.Fatalf("all = %v, want %v", rs.all, tc.wantAll)
			}
			if tc.wantAll {
				return
			}
			for _, n := range tc.want {
				if !rs.Has(n) {
					t.Errorf("Has(%q) = false, want true", n)
				}
			}
			if got := len(rs.names); got != len(tc.want) {
				t.Errorf("len(names) = %d, want %d", got, len(tc.want))
			}
		})
	}
}

func TestRoleSetHas(t *testing.T) {
	all := roleSet{all: true}
	if !all.Has("anything") {
		t.Error("monolith sentinel should host anything")
	}
	sub := parseRoles("inventory")
	if !sub.Has("inventory") {
		t.Error("Has(inventory) should be true")
	}
	if sub.Has("accounts") {
		t.Error("Has(accounts) should be false for ROLES=inventory")
	}
}

func TestPlanModules(t *testing.T) {
	all := realModules()

	tests := []struct {
		name       string
		roles      string
		wantHosted []string
		wantNeeded []string
	}{
		{
			name:       "monolith hosts all, no stubs",
			roles:      "",
			wantHosted: []string{"accounts", "admin", "characters", "inventory", "leaderboard", "match", "rating", "webui"},
			wantNeeded: []string{},
		},
		{
			name:       "accounts+characters — dep satisfied locally",
			roles:      "accounts,characters",
			wantHosted: []string{"accounts", "characters"},
			wantNeeded: []string{},
		},
		{
			name:       "inventory+admin — two stubs for accounts+characters",
			roles:      "inventory,admin",
			wantHosted: []string{"admin", "inventory"},
			wantNeeded: []string{"accounts", "characters"},
		},
		{
			name:       "match alone needs rating stub",
			roles:      "match",
			wantHosted: []string{"match"},
			wantNeeded: []string{"rating"},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			hosted, needed, err := planModules(parseRoles(tc.roles), all)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if !reflect.DeepEqual(hosted, tc.wantHosted) {
				t.Errorf("hosted = %v, want %v", hosted, tc.wantHosted)
			}
			if !reflect.DeepEqual(needed, tc.wantNeeded) {
				t.Errorf("needed = %v, want %v", needed, tc.wantNeeded)
			}
		})
	}
}

func TestPlanModulesUnknownRole(t *testing.T) {
	_, _, err := planModules(parseRoles("nope"), realModules())
	if err == nil {
		t.Fatal("expected error for unknown role")
	}
}

func TestNormalizeAddr(t *testing.T) {
	tests := map[string]string{
		"":       ":8080",
		"  ":     ":8080",
		"8080":   ":8080",
		":8080":  ":8080",
		"9000":   ":9000",
		":9000":  ":9000",
		" 8081 ": ":8081",
	}
	for in, want := range tests {
		if got := normalizeAddr(in); got != want {
			t.Errorf("normalizeAddr(%q) = %q, want %q", in, got, want)
		}
	}
}
