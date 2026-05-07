package scaleway

import (
	"context"
	"fmt"
	"strings"

	baremetal "github.com/scaleway/scaleway-sdk-go/api/baremetal/v1"
	"github.com/scaleway/scaleway-sdk-go/scw"
)

func resolveOfferForBillingCycle(ctx context.Context, client *Client, zone scw.Zone, offerID, billingCycle string) (string, baremetal.OfferSubscriptionPeriod, error) {
	offerID = strings.TrimSpace(offerID)
	if offerID == "" {
		return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, fmt.Errorf("offer ID/name is required")
	}

	cycle := strings.TrimSpace(strings.ToLower(billingCycle))
	if cycle == "" {
		cycle = "hourly"
	}

	if !isLikelyUUID(offerID) {
		var period baremetal.OfferSubscriptionPeriod
		switch cycle {
		case "hourly":
			period = baremetal.OfferSubscriptionPeriodHourly
		case "monthly":
			period = baremetal.OfferSubscriptionPeriodMonthly
		default:
			return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, fmt.Errorf("invalid billing cycle %q", billingCycle)
		}
		resolvedID, err := resolveOfferIDFromNameByPeriod(ctx, client, zone, offerID, period)
		if err != nil {
			return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, err
		}
		return resolvedID, period, nil
	}

	offer, err := client.Baremetal.GetOffer(&baremetal.GetOfferRequest{
		Zone:    zone,
		OfferID: offerID,
	}, scw.WithContext(ctx))
	if err != nil {
		return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, fmt.Errorf("get offer %s: %w", offerID, err)
	}

	switch cycle {
	case "hourly":
		if offer.SubscriptionPeriod == baremetal.OfferSubscriptionPeriodHourly {
			return offer.ID, offer.SubscriptionPeriod, nil
		}
		hourlyOfferID, err := resolveOfferIDByNameAndPeriod(ctx, client, zone, offer, baremetal.OfferSubscriptionPeriodHourly)
		if err != nil {
			return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, err
		}
		return hourlyOfferID, baremetal.OfferSubscriptionPeriodHourly, nil
	case "monthly":
		if offer.SubscriptionPeriod == baremetal.OfferSubscriptionPeriodMonthly {
			return offer.ID, offer.SubscriptionPeriod, nil
		}
		if offer.MonthlyOfferID != nil && *offer.MonthlyOfferID != "" {
			return *offer.MonthlyOfferID, baremetal.OfferSubscriptionPeriodMonthly, nil
		}
		monthlyOfferID, err := resolveOfferIDByNameAndPeriod(ctx, client, zone, offer, baremetal.OfferSubscriptionPeriodMonthly)
		if err != nil {
			return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, err
		}
		return monthlyOfferID, baremetal.OfferSubscriptionPeriodMonthly, nil
	default:
		return "", baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod, fmt.Errorf("invalid billing cycle %q", billingCycle)
	}
}

func resolveOfferIDFromNameByPeriod(ctx context.Context, client *Client, zone scw.Zone, offerName string, period baremetal.OfferSubscriptionPeriod) (string, error) {
	trimmedName := strings.TrimSpace(offerName)
	resp, err := client.Baremetal.ListOffers(&baremetal.ListOffersRequest{
		Zone:               zone,
		SubscriptionPeriod: period,
		Name:               &trimmedName,
	}, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return "", fmt.Errorf("list offers for %q (%s): %w", offerName, period, err)
	}

	if len(resp.Offers) == 0 {
		return "", fmt.Errorf("no %s offer found matching %q", period, offerName)
	}

	normalizedQuery := normalizeOfferName(offerName)
	for _, offer := range resp.Offers {
		if offer == nil || offer.ID == "" {
			continue
		}
		if strings.EqualFold(offer.Name, trimmedName) {
			return offer.ID, nil
		}
	}
	for _, offer := range resp.Offers {
		if offer == nil || offer.ID == "" {
			continue
		}
		if normalizeOfferName(offer.Name) == normalizedQuery {
			return offer.ID, nil
		}
	}
	for _, offer := range resp.Offers {
		if offer == nil || offer.ID == "" {
			continue
		}
		return offer.ID, nil
	}

	return "", fmt.Errorf("no usable %s offer found for %q", period, offerName)
}

func resolveOfferIDByNameAndPeriod(ctx context.Context, client *Client, zone scw.Zone, baseOffer *baremetal.Offer, period baremetal.OfferSubscriptionPeriod) (string, error) {
	if baseOffer == nil {
		return "", fmt.Errorf("base offer is nil")
	}

	name := baseOffer.Name
	resp, err := client.Baremetal.ListOffers(&baremetal.ListOffersRequest{
		Zone:               zone,
		SubscriptionPeriod: period,
		Name:               &name,
	}, scw.WithAllPages(), scw.WithContext(ctx))
	if err != nil {
		return "", fmt.Errorf("list %s offers for %q: %w", period, baseOffer.Name, err)
	}

	for _, candidate := range resp.Offers {
		if candidate == nil || candidate.ID == "" || candidate.ID == baseOffer.ID {
			continue
		}
		if candidate.Name == baseOffer.Name {
			return candidate.ID, nil
		}
	}
	for _, candidate := range resp.Offers {
		if candidate == nil || candidate.ID == "" || candidate.ID == baseOffer.ID {
			continue
		}
		return candidate.ID, nil
	}

	return "", fmt.Errorf("could not find %s variant for offer %q (%s)", period, baseOffer.Name, baseOffer.ID)
}

func isLikelyUUID(value string) bool {
	if len(value) != 36 {
		return false
	}
	for i, ch := range value {
		switch i {
		case 8, 13, 18, 23:
			if ch != '-' {
				return false
			}
		default:
			if (ch < '0' || ch > '9') && (ch < 'a' || ch > 'f') && (ch < 'A' || ch > 'F') {
				return false
			}
		}
	}
	return true
}

func normalizeOfferName(value string) string {
	trimmed := strings.TrimSpace(strings.ToLower(value))
	var b strings.Builder
	b.Grow(len(trimmed))
	for _, ch := range trimmed {
		if (ch >= 'a' && ch <= 'z') || (ch >= '0' && ch <= '9') {
			b.WriteRune(ch)
		}
	}
	return b.String()
}

func privateNetworkOptionIDsForOffer(offer *baremetal.Offer, period baremetal.OfferSubscriptionPeriod) ([]string, bool, error) {
	if offer == nil {
		return nil, false, fmt.Errorf("offer is required")
	}

	var candidates []*baremetal.OfferOptionOffer
	for _, option := range offer.Options {
		if option == nil || option.PrivateNetwork == nil {
			continue
		}
		if period != baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod &&
			option.SubscriptionPeriod != baremetal.OfferSubscriptionPeriodUnknownSubscriptionPeriod &&
			option.SubscriptionPeriod != period {
			continue
		}
		candidates = append(candidates, option)
	}
	if len(candidates) == 0 {
		return nil, false, fmt.Errorf("offer %s (%s) does not expose a private-network option", offer.Name, offer.ID)
	}

	for _, option := range candidates {
		if option.Enabled {
			return nil, true, nil
		}
	}

	for _, option := range candidates {
		if !option.Manageable {
			continue
		}
		optionID := strings.TrimSpace(option.ID)
		if optionID == "" {
			continue
		}
		return []string{optionID}, false, nil
	}

	return nil, false, fmt.Errorf("offer %s (%s) has no manageable private-network option to enable", offer.Name, offer.ID)
}
