package match

import (
	"encoding/json"
	"net/http"

	"gamebackend/core"
	"gamebackend/modules/match/matchevents"
)

// ratingService is the SLICE of "rating" this module actually needs. Declaring
// it locally means match depends on a capability, not on the rating package.
type ratingService interface {
	MMR(playerID string) int
}

type Module struct{}

func (Module) Name() string        { return "match" }
func (Module) DependsOn() []string { return []string{"rating"} } // needs a synchronous answer

func (Module) Init(ctx *core.Context) error {
	rs := ctx.Require("rating").(ratingService) // assert to our local interface

	ctx.Mux.HandleFunc("POST /match/report", func(w http.ResponseWriter, r *http.Request) {
		var in struct{ Winner, Loser string }
		if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		// Synchronous use of a dependency: query MMR right now.
		ctx.Log.Info("match reported",
			"winner", in.Winner, "winnerMMR", rs.MMR(in.Winner), "loser", in.Loser)

		// Fire-and-forget: announce it happened — whoever cares subscribes.
		ctx.Bus.Publish(core.Event{
			Topic: matchevents.TopicFinished,
			Data:  matchevents.Finished{Winner: in.Winner, Loser: in.Loser},
		})
		w.WriteHeader(http.StatusAccepted)
	})
	return nil
}
