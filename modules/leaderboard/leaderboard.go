package leaderboard

import (
	"encoding/json"
	"net/http"
	"sort"
	"sync"

	"gamebackend/core"
	"gamebackend/modules/match/matchevents"
)

type Module struct{}

func (Module) Name() string        { return "leaderboard" }
func (Module) DependsOn() []string { return nil } // reacts via the bus — depends on nobody

func (Module) Init(ctx *core.Context) error {
	var mu sync.Mutex
	wins := map[string]int{}

	core.On(ctx.Bus, matchevents.FinishedEvent, func(r matchevents.Finished) {
		mu.Lock()
		wins[r.Winner]++
		mu.Unlock()
	})

	ctx.Mux.HandleFunc("GET /leaderboard", func(w http.ResponseWriter, _ *http.Request) {
		type row struct {
			Player string `json:"player"`
			Wins   int    `json:"wins"`
		}
		mu.Lock()
		rows := make([]row, 0, len(wins))
		for p, n := range wins {
			rows = append(rows, row{p, n})
		}
		mu.Unlock()
		sort.Slice(rows, func(i, j int) bool { return rows[i].Wins > rows[j].Wins })
		json.NewEncoder(w).Encode(rows)
	})
	return nil
}
