//! Published usage rates selected at upstream attempt admission.
use crate::{
    calendar::{Calendar, Schedule},
    config::{BackendModelConfig, ModelPricingConfig},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PricingSchedule {
    pub timezone: String,
    pub windows: Vec<PriceWindow>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PriceWindow {
    pub name: String,
    pub days: Vec<u8>,
    pub start: String,
    pub end: String,
    pub pricing: ModelPricingConfig,
}

#[derive(Clone, Debug)]
pub struct Tariff {
    calendar: Calendar,
    rates: Vec<Option<ModelPricingConfig>>,
}

impl Tariff {
    pub fn new(model: &BackendModelConfig) -> Result<Self, String> {
        let mut rates = vec![model.pricing];
        let mut labels = vec!["default".to_owned()];
        if let Some(schedule) = &model.pricing_schedule {
            if model.pricing.is_none() {
                return Err("price windows require default usage rates".into());
            }
            for (index, window) in schedule.windows.iter().enumerate() {
                labels.push(format!("window-{index}"));
                rates.push(Some(window.pricing));
            }
        }
        for price in rates.iter().flatten() {
            if [
                price.cache_hit_input_usd_per_million,
                price.cache_miss_input_usd_per_million,
                price.output_usd_per_million,
            ]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
            {
                return Err("usage rates must be finite and non-negative".into());
            }
        }
        let schedules = model
            .pricing_schedule
            .as_ref()
            .map(|schedule| {
                schedule
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(index, window)| {
                        let mut order = vec![labels[index + 1].clone()];
                        order.extend(
                            labels
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| *i != index + 1)
                                .map(|(_, label)| label.clone()),
                        );
                        Schedule {
                            name: window.name.clone(),
                            days: window.days.clone(),
                            start: window.start.clone(),
                            end: window.end.clone(),
                            order,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let timezone = model
            .pricing_schedule
            .as_ref()
            .map(|s| s.timezone.as_str())
            .unwrap_or("UTC");
        Ok(Self {
            calendar: Calendar::new(timezone, &labels, &schedules)?,
            rates,
        })
    }

    pub fn at(&self, at: DateTime<Utc>) -> (Option<String>, Option<ModelPricingConfig>) {
        let selected = self.calendar.at(at);
        (selected.active_schedule, self.rates[selected.order[0]])
    }
}
