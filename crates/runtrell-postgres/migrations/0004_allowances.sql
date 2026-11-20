-- SPDX-License-Identifier: Apache-2.0
CREATE TABLE IF NOT EXISTS {{schema}}.allowance_periods (
    allowance text NOT NULL,
    scope text NOT NULL,
    period text NOT NULL,
    capacity numeric(20,0) NOT NULL CHECK (capacity BETWEEN 0 AND 18446744073709551615),
    consumed numeric(20,0) NOT NULL DEFAULT 0 CHECK (consumed BETWEEN 0 AND capacity),
    PRIMARY KEY (allowance, scope, period)
);
CREATE TABLE IF NOT EXISTS {{schema}}.allowance_receipts (
    allowance text NOT NULL,
    scope text NOT NULL,
    period text NOT NULL,
    receipt text NOT NULL,
    quantity numeric(20,0) NOT NULL CHECK (quantity BETWEEN 0 AND 18446744073709551615),
    included numeric(20,0) NOT NULL CHECK (included BETWEEN 0 AND quantity),
    PRIMARY KEY (allowance, scope, period, receipt),
    FOREIGN KEY (allowance, scope, period) REFERENCES {{schema}}.allowance_periods
);
