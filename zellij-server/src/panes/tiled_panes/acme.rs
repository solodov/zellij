use std::collections::HashSet;

use crate::panes::PaneId;
use zellij_utils::{
    data::{Direction, PaletteColor, Resize, ResizeStrategy},
    errors::prelude::*,
    pane_size::{Dimension, PaneGeom, Viewport},
};

pub(super) const ACME_COLLAPSED_PANE_ROWS: usize = 1;
pub(super) const ACME_TITLE_BUTTON_COLUMN_OFFSET: usize = 1;
pub(super) const ACME_BOUNDARY_COLOR: Option<(PaletteColor, usize)> =
    Some((PaletteColor::Rgb((0x5a, 0xae, 0xc6)), 0));

#[derive(Clone, Debug)]
pub(super) struct AcmePaneGeometry {
    pub(super) pane_id: PaneId,
    pub(super) geom: PaneGeom,
}

#[derive(Clone, Debug)]
pub(super) struct AcmeColumn {
    pub(super) x: usize,
    pub(super) cols: usize,
    pub(super) pane_geometries: Vec<AcmePaneGeometry>,
}

impl AcmeColumn {
    pub(super) fn contains_pane(&self, pane_id: PaneId) -> bool {
        self.pane_geometries
            .iter()
            .any(|pane_geometry| pane_geometry.pane_id == pane_id)
    }

    pub(super) fn pane_ids(&self) -> Vec<PaneId> {
        self.pane_geometries
            .iter()
            .map(|pane_geometry| pane_geometry.pane_id)
            .collect()
    }
}

#[derive(Clone, Debug)]
pub(super) struct AcmePaneRowsSnapshot {
    target_pane_id: PaneId,
    pane_rows: Vec<(PaneId, Dimension)>,
}

impl AcmePaneRowsSnapshot {
    pub(super) fn new(column: &AcmeColumn, target_pane_id: PaneId) -> Self {
        AcmePaneRowsSnapshot {
            target_pane_id,
            pane_rows: column
                .pane_geometries
                .iter()
                .map(|pane_geometry| (pane_geometry.pane_id, pane_geometry.geom.rows))
                .collect(),
        }
    }

    pub(super) fn matches_column(&self, column: &AcmeColumn) -> bool {
        self.pane_ids() == column.pane_ids()
    }

    fn pane_ids(&self) -> Vec<PaneId> {
        self.pane_rows
            .iter()
            .map(|(pane_id, _rows)| *pane_id)
            .collect()
    }
}

pub(super) fn percent_dimension(inner: usize, full_size: usize) -> Dimension {
    let percent = if full_size == 0 {
        100.0
    } else {
        (inner as f64 / full_size as f64) * 100.0
    };
    let mut dimension = Dimension::percent(percent);
    dimension.set_inner(inner);
    dimension
}

pub(super) fn equalized_lengths(total_size: usize, item_count: usize) -> Vec<usize> {
    if item_count == 0 {
        return vec![];
    }
    let base_size = total_size / item_count;
    let remainder = total_size % item_count;
    (0..item_count)
        .map(|index| base_size + usize::from(index < remainder))
        .collect()
}

pub(super) fn equalized_acme_row_heights(
    existing_rows: &[usize],
    viewport_rows: usize,
) -> Vec<usize> {
    let collapsed_pane_count = existing_rows
        .iter()
        .filter(|&&rows| rows == ACME_COLLAPSED_PANE_ROWS)
        .count();
    let expanded_pane_count = existing_rows.len().saturating_sub(collapsed_pane_count);
    if expanded_pane_count == 0 {
        return equalized_lengths(viewport_rows, existing_rows.len());
    }

    let expanded_rows_total = viewport_rows
        .saturating_sub(collapsed_pane_count * ACME_COLLAPSED_PANE_ROWS);
    let mut expanded_rows = equalized_lengths(expanded_rows_total, expanded_pane_count).into_iter();
    existing_rows
        .iter()
        .map(|&rows| {
            if rows == ACME_COLLAPSED_PANE_ROWS {
                ACME_COLLAPSED_PANE_ROWS
            } else {
                expanded_rows.next().unwrap_or(ACME_COLLAPSED_PANE_ROWS)
            }
        })
        .collect()
}

pub(super) fn acme_title_pane_ids(columns: &[AcmeColumn]) -> HashSet<PaneId> {
    columns
        .iter()
        .flat_map(|column| column.pane_geometries.iter())
        .map(|pane_geometry| pane_geometry.pane_id)
        .collect()
}

pub(super) fn acme_own_line_title_pane_ids(columns: &[AcmeColumn]) -> HashSet<PaneId> {
    let previous_line_title_pane_ids = acme_previous_line_title_pane_ids(columns);
    acme_title_pane_ids(columns)
        .difference(&previous_line_title_pane_ids)
        .copied()
        .collect()
}

pub(super) fn acme_panes_before_own_line_title(
    columns: &[AcmeColumn],
    own_line_title_pane_ids: &HashSet<PaneId>,
) -> HashSet<PaneId> {
    columns
        .iter()
        .flat_map(|column| column.pane_geometries.windows(2))
        .filter(|pane_pair| own_line_title_pane_ids.contains(&pane_pair[1].pane_id))
        .map(|pane_pair| pane_pair[0].pane_id)
        .collect()
}

pub(super) fn acme_own_line_title_boundary_segments(
    columns: &[AcmeColumn],
) -> Vec<(usize, usize, usize)> {
    let own_line_title_pane_ids = acme_own_line_title_pane_ids(columns);
    columns
        .iter()
        .flat_map(|column| {
            column
                .pane_geometries
                .iter()
                .skip(1)
                .filter(|pane_geometry| own_line_title_pane_ids.contains(&pane_geometry.pane_id))
                .map(|pane_geometry| {
                    let x = column.x.saturating_sub(1);
                    let width = column.cols + usize::from(column.x > 0);
                    (x, pane_geometry.geom.y.saturating_sub(1), width)
                })
        })
        .collect()
}

pub(super) fn acme_previous_line_title_pane_ids(_columns: &[AcmeColumn]) -> HashSet<PaneId> {
    HashSet::new()
}

pub(super) fn acme_focus_target_after_removing_pane(
    columns: &[AcmeColumn],
    pane_id: PaneId,
) -> Option<PaneId> {
    columns
        .iter()
        .find_map(|column| acme_neighbor_focus_target(column, pane_id))
}

fn acme_neighbor_focus_target(column: &AcmeColumn, pane_id: PaneId) -> Option<PaneId> {
    let pane_index = column
        .pane_geometries
        .iter()
        .position(|pane_geometry| pane_geometry.pane_id == pane_id)?;
    if column.pane_geometries.len() == 1 {
        None
    } else if pane_index > 0 {
        Some(column.pane_geometries[pane_index - 1].pane_id)
    } else {
        Some(column.pane_geometries[pane_index + 1].pane_id)
    }
}

pub(super) fn acme_pane_is_maximized(column: &AcmeColumn, pane_id: PaneId) -> bool {
    column.pane_geometries.len() > 1
        && column.pane_geometries.iter().all(|pane_geometry| {
            let rows = pane_geometry.geom.rows.as_usize();
            if pane_geometry.pane_id == pane_id {
                rows > ACME_COLLAPSED_PANE_ROWS
            } else {
                rows == ACME_COLLAPSED_PANE_ROWS
            }
        })
}

pub(super) fn acme_geometries_after_maximizing_pane(
    columns: &[AcmeColumn],
    target_pane_id: PaneId,
    viewport: Viewport,
) -> Result<Vec<(PaneId, PaneGeom)>> {
    let column = columns
        .iter()
        .find(|column| column.contains_pane(target_pane_id))
        .ok_or_else(|| anyhow!("Focused pane is not in an Acme column"))?;
    if column.pane_geometries.len() == 1 {
        return Ok(vec![]);
    }
    let collapsed_rows = (column.pane_geometries.len() - 1) * ACME_COLLAPSED_PANE_ROWS;
    if viewport.rows <= collapsed_rows {
        return Err(anyhow!("Not enough room to maximize Acme pane"));
    }
    let focused_rows = viewport.rows - collapsed_rows;
    let mut next_y = viewport.y;
    let mut planned_geometries = vec![];
    for pane_geometry in &column.pane_geometries {
        let mut geom = pane_geometry.geom;
        geom.y = next_y;
        geom.stacked = None;
        if pane_geometry.pane_id == target_pane_id {
            geom.rows = percent_dimension(focused_rows, viewport.rows);
            next_y += focused_rows;
        } else {
            geom.rows = Dimension::fixed(ACME_COLLAPSED_PANE_ROWS);
            next_y += ACME_COLLAPSED_PANE_ROWS;
        }
        planned_geometries.push((pane_geometry.pane_id, geom));
    }
    Ok(planned_geometries)
}

pub(super) fn acme_geometries_after_restoring_pane_rows(
    columns: &[AcmeColumn],
    snapshot: &AcmePaneRowsSnapshot,
    viewport: Viewport,
) -> Result<Vec<(PaneId, PaneGeom)>> {
    let column = columns
        .iter()
        .find(|column| column.contains_pane(snapshot.target_pane_id))
        .ok_or_else(|| anyhow!("Focused pane is not in an Acme column"))?;
    if column.pane_ids() != snapshot.pane_ids() {
        return Ok(vec![]);
    }

    let restored_row_count: usize = snapshot
        .pane_rows
        .iter()
        .map(|(_pane_id, rows)| rows.as_usize())
        .sum();
    if restored_row_count != viewport.rows {
        return Ok(vec![]);
    }

    let mut next_y = viewport.y;
    let mut planned_geometries = vec![];
    for (pane_geometry, (_pane_id, rows)) in
        column.pane_geometries.iter().zip(&snapshot.pane_rows)
    {
        let mut geom = pane_geometry.geom;
        geom.y = next_y;
        geom.rows = *rows;
        geom.stacked = None;
        next_y += rows.as_usize();
        planned_geometries.push((pane_geometry.pane_id, geom));
    }
    Ok(planned_geometries)
}

pub(super) fn acme_geometries_after_minimizing_pane(
    columns: &[AcmeColumn],
    target_pane_id: PaneId,
    viewport: Viewport,
) -> Result<(Vec<(PaneId, PaneGeom)>, Option<PaneId>)> {
    let column = columns
        .iter()
        .find(|column| column.contains_pane(target_pane_id))
        .ok_or_else(|| anyhow!("Focused pane is not in an Acme column"))?;
    if column.pane_geometries.len() == 1 {
        return Ok((vec![], None));
    }
    let remaining_rows = viewport.rows.saturating_sub(ACME_COLLAPSED_PANE_ROWS);
    let expanded_pane_count = column.pane_geometries.len() - 1;
    if remaining_rows < expanded_pane_count * ACME_COLLAPSED_PANE_ROWS {
        return Err(anyhow!("Not enough room to minimize Acme pane"));
    }
    let mut expanded_rows = equalized_lengths(remaining_rows, expanded_pane_count).into_iter();
    let mut next_y = viewport.y;
    let mut planned_geometries = vec![];
    for pane_geometry in &column.pane_geometries {
        let mut geom = pane_geometry.geom;
        geom.y = next_y;
        geom.stacked = None;
        let rows = if pane_geometry.pane_id == target_pane_id {
            ACME_COLLAPSED_PANE_ROWS
        } else {
            expanded_rows
                .next()
                .ok_or_else(|| anyhow!("Missing Acme pane row height"))?
        };
        geom.rows = acme_rows_dimension(rows, viewport.rows);
        next_y += rows;
        planned_geometries.push((pane_geometry.pane_id, geom));
    }
    if next_y != viewport.y + viewport.rows {
        return Err(anyhow!("Acme column does not fill the viewport height"));
    }
    let focus_target = acme_neighbor_focus_target(column, target_pane_id);
    Ok((planned_geometries, focus_target))
}

/// Plan a mouse-driven Acme pane move without mutating live panes.
pub(super) fn acme_geometries_after_moving_pane_to_position(
    columns: &[AcmeColumn],
    pane_id: PaneId,
    line: isize,
    position_column: usize,
    viewport: Viewport,
    minimum_pane_rows: usize,
) -> Result<Vec<(PaneId, PaneGeom)>> {
    if line < viewport.y as isize
        || line >= (viewport.y + viewport.rows) as isize
        || position_column < viewport.x
        || position_column >= viewport.x + viewport.cols
    {
        return Ok(vec![]);
    }

    let source_column_index = columns
        .iter()
        .position(|column| column.contains_pane(pane_id))
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;
    let target_column_index = match acme_column_index_at_position(columns, position_column) {
        Some(target_column_index) => target_column_index,
        None => return Ok(vec![]),
    };
    if source_column_index == target_column_index {
        return Ok(vec![]);
    }

    let target_pane_count = columns[target_column_index].pane_geometries.len() + 1;
    if viewport.rows < target_pane_count * minimum_pane_rows {
        return Err(anyhow!("Not enough room to move Acme pane"));
    }

    let target_insert_index = acme_insert_index_at_line(&columns[target_column_index], line);
    let mut columns = columns.to_vec();
    let source_pane_index = columns[source_column_index]
        .pane_geometries
        .iter()
        .position(|pane_geometry| pane_geometry.pane_id == pane_id)
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;
    let moving_pane = columns[source_column_index]
        .pane_geometries
        .remove(source_pane_index);

    let mut target_column_index = target_column_index;
    let source_column_removed = columns[source_column_index].pane_geometries.is_empty();
    if source_column_removed {
        let removed_column = columns.remove(source_column_index);
        if target_column_index > source_column_index {
            target_column_index -= 1;
        }
        expand_acme_columns_after_removing_column(
            &mut columns,
            source_column_index,
            removed_column.cols,
            viewport,
        )?;
    }

    let target_column = columns
        .get_mut(target_column_index)
        .ok_or_else(|| anyhow!("Target Acme column disappeared while moving pane"))?;
    let target_insert_index = target_insert_index.min(target_column.pane_geometries.len());
    target_column
        .pane_geometries
        .insert(target_insert_index, moving_pane);

    if !source_column_removed {
        equalize_acme_column_rows(&mut columns[source_column_index], viewport)?;
    }
    equalize_acme_column_rows(&mut columns[target_column_index], viewport)?;
    Ok(planned_geometries_for_columns(columns, viewport))
}

/// Plan a same-column Acme pane reorder without mutating live panes.
pub(super) fn acme_geometries_after_reordering_pane(
    columns: &[AcmeColumn],
    pane_id: PaneId,
    start_line: isize,
    release_line: isize,
    release_column: usize,
    viewport: Viewport,
) -> Result<Vec<(PaneId, PaneGeom)>> {
    if release_line < viewport.y as isize
        || release_line >= (viewport.y + viewport.rows) as isize
        || release_column < viewport.x
        || release_column >= viewport.x + viewport.cols
    {
        return Ok(vec![]);
    }

    let source_column_index = columns
        .iter()
        .position(|column| column.contains_pane(pane_id))
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;
    let target_column_index = match acme_column_index_at_position(columns, release_column) {
        Some(target_column_index) => target_column_index,
        None => return Ok(vec![]),
    };
    if source_column_index != target_column_index || release_line == start_line {
        return Ok(vec![]);
    }

    let target_pane_id = match acme_pane_index_at_line(&columns[source_column_index], release_line)
    {
        Some(target_pane_index) => columns[source_column_index].pane_geometries[target_pane_index]
            .pane_id,
        None => return Ok(vec![]),
    };
    if target_pane_id == pane_id {
        return Ok(vec![]);
    }

    let mut columns = columns.to_vec();
    let column = &mut columns[source_column_index];
    let original_pane_ids = column.pane_ids();
    let source_pane_index = column
        .pane_geometries
        .iter()
        .position(|pane_geometry| pane_geometry.pane_id == pane_id)
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;
    let moving_pane = column.pane_geometries.remove(source_pane_index);
    let target_pane_index = column
        .pane_geometries
        .iter()
        .position(|pane_geometry| pane_geometry.pane_id == target_pane_id)
        .ok_or_else(|| anyhow!("Target Acme pane disappeared while reordering"))?;
    // Dropping onto another pane places the moved pane on the side it approached from.
    let insert_index = if release_line > start_line {
        target_pane_index + 1
    } else {
        target_pane_index
    }
    .min(column.pane_geometries.len());
    column.pane_geometries.insert(insert_index, moving_pane);
    if column.pane_ids() == original_pane_ids {
        return Ok(vec![]);
    }

    stack_acme_column_rows(column, viewport)?;
    Ok(planned_geometries_for_columns(columns, viewport))
}

pub(super) fn acme_rows_dimension(rows: usize, viewport_rows: usize) -> Dimension {
    if rows == ACME_COLLAPSED_PANE_ROWS {
        Dimension::fixed(ACME_COLLAPSED_PANE_ROWS)
    } else {
        percent_dimension(rows, viewport_rows)
    }
}

pub(super) fn resize_acme_column_pane(
    column: &mut AcmeColumn,
    pane_index: usize,
    strategy: &ResizeStrategy,
    row_delta: usize,
    viewport: Viewport,
) -> Result<bool> {
    let mut row_heights: Vec<usize> = column
        .pane_geometries
        .iter()
        .map(|pane_geometry| pane_geometry.geom.rows.as_usize())
        .collect();
    if pane_index >= row_heights.len() {
        return Ok(false);
    }
    let pane_count = row_heights.len();

    let changed = match (strategy.resize, strategy.direction) {
        (Resize::Increase, Some(Direction::Up)) => transfer_rows_between_acme_panes(
            &mut row_heights,
            (0..pane_index).rev(),
            pane_index,
            row_delta,
        ),
        (Resize::Decrease, Some(Direction::Up)) => {
            let Some(recipient_index) = pane_index.checked_sub(1) else {
                return Ok(false);
            };
            transfer_rows_between_acme_panes(
                &mut row_heights,
                pane_index..pane_count,
                recipient_index,
                row_delta,
            )
        },
        (Resize::Increase, Some(Direction::Down)) => transfer_rows_between_acme_panes(
            &mut row_heights,
            pane_index + 1..pane_count,
            pane_index,
            row_delta,
        ),
        (Resize::Decrease, Some(Direction::Down)) => {
            let recipient_index = pane_index + 1;
            if recipient_index >= pane_count {
                return Ok(false);
            }
            transfer_rows_between_acme_panes(
                &mut row_heights,
                (0..=pane_index).rev(),
                recipient_index,
                row_delta,
            )
        },
        _ => false,
    };
    if !changed {
        return Ok(false);
    }

    let mut next_y = viewport.y;
    for (pane_geometry, rows) in column.pane_geometries.iter_mut().zip(row_heights) {
        pane_geometry.geom.y = next_y;
        pane_geometry.geom.rows = acme_rows_dimension(rows, viewport.rows);
        pane_geometry.geom.stacked = None;
        next_y += rows;
    }
    if next_y != viewport.y + viewport.rows {
        return Err(anyhow!("Acme column does not fill the viewport height"));
    }
    Ok(true)
}

fn transfer_rows_between_acme_panes(
    row_heights: &mut [usize],
    donor_indices: impl IntoIterator<Item = usize>,
    recipient_index: usize,
    row_delta: usize,
) -> bool {
    if recipient_index >= row_heights.len() {
        return false;
    }
    let mut remaining_rows = row_delta;
    let mut changed = false;
    for donor_index in donor_indices {
        if donor_index == recipient_index || donor_index >= row_heights.len() {
            continue;
        }
        let rows_to_transfer = remaining_rows.min(
            row_heights[donor_index].saturating_sub(ACME_COLLAPSED_PANE_ROWS),
        );
        if rows_to_transfer == 0 {
            continue;
        }
        row_heights[donor_index] -= rows_to_transfer;
        row_heights[recipient_index] += rows_to_transfer;
        remaining_rows -= rows_to_transfer;
        changed = true;
        if remaining_rows == 0 {
            break;
        }
    }
    changed
}

pub(super) fn acme_geometries_after_removing_pane(
    columns: &[AcmeColumn],
    pane_id: PaneId,
    viewport: Viewport,
) -> Result<Vec<(PaneId, PaneGeom)>> {
    let mut columns = columns.to_vec();
    let target_column_index = columns
        .iter()
        .position(|column| column.contains_pane(pane_id))
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;
    let target_pane_index = columns[target_column_index]
        .pane_geometries
        .iter()
        .position(|pane_geometry| pane_geometry.pane_id == pane_id)
        .ok_or_else(|| anyhow!("Pane is not in an Acme column"))?;

    if columns[target_column_index].pane_geometries.len() == 1 {
        let removed_column = columns.remove(target_column_index);
        if columns.is_empty() {
            return Ok(vec![]);
        }
        expand_acme_columns_after_removing_column(
            &mut columns,
            target_column_index,
            removed_column.cols,
            viewport,
        )?;
    } else {
        resize_acme_column_after_removing_pane(
            &mut columns[target_column_index],
            target_pane_index,
            viewport,
        )?;
    }

    Ok(planned_geometries_for_columns(columns, viewport))
}

fn acme_column_index_at_position(columns: &[AcmeColumn], position_column: usize) -> Option<usize> {
    columns
        .iter()
        .position(|column| position_column >= column.x && position_column < column.x + column.cols)
}

fn acme_insert_index_at_line(column: &AcmeColumn, line: isize) -> usize {
    acme_pane_index_at_line(column, line)
        .map(|pane_index| pane_index + 1)
        .unwrap_or(column.pane_geometries.len())
}

fn acme_pane_index_at_line(column: &AcmeColumn, line: isize) -> Option<usize> {
    column.pane_geometries.iter().position(|pane_geometry| {
        let pane_top = pane_geometry.geom.y as isize;
        let pane_bottom = (pane_geometry.geom.y + pane_geometry.geom.rows.as_usize()) as isize;
        line >= pane_top && line < pane_bottom
    })
}

fn equalize_acme_column_rows(column: &mut AcmeColumn, viewport: Viewport) -> Result<()> {
    let existing_rows: Vec<usize> = column
        .pane_geometries
        .iter()
        .map(|pane_geometry| pane_geometry.geom.rows.as_usize())
        .collect();
    let rows = equalized_acme_row_heights(&existing_rows, viewport.rows);
    let mut next_y = viewport.y;
    for (pane_geometry, rows) in column.pane_geometries.iter_mut().zip(rows) {
        pane_geometry.geom.y = next_y;
        pane_geometry.geom.rows = acme_rows_dimension(rows, viewport.rows);
        pane_geometry.geom.stacked = None;
        next_y += rows;
    }
    if next_y != viewport.y + viewport.rows {
        return Err(anyhow!("Acme column does not fill the viewport height"));
    }
    Ok(())
}

fn stack_acme_column_rows(column: &mut AcmeColumn, viewport: Viewport) -> Result<()> {
    let mut next_y = viewport.y;
    for pane_geometry in &mut column.pane_geometries {
        let rows = pane_geometry.geom.rows.as_usize();
        pane_geometry.geom.y = next_y;
        pane_geometry.geom.stacked = None;
        next_y += rows;
    }
    if next_y != viewport.y + viewport.rows {
        return Err(anyhow!("Acme column does not fill the viewport height"));
    }
    Ok(())
}

fn planned_geometries_for_columns(
    columns: Vec<AcmeColumn>,
    viewport: Viewport,
) -> Vec<(PaneId, PaneGeom)> {
    let mut planned_geometries = vec![];
    for column in columns {
        for pane_geometry in column.pane_geometries {
            let mut geom = pane_geometry.geom;
            geom.x = column.x;
            geom.cols = percent_dimension(column.cols, viewport.cols);
            geom.stacked = None;
            planned_geometries.push((pane_geometry.pane_id, geom));
        }
    }
    planned_geometries
}

fn resize_acme_column_after_removing_pane(
    column: &mut AcmeColumn,
    pane_index: usize,
    viewport: Viewport,
) -> Result<()> {
    let removed_pane = column.pane_geometries.remove(pane_index);
    let mut row_heights: Vec<usize> = column
        .pane_geometries
        .iter()
        .map(|pane_geometry| pane_geometry.geom.rows.as_usize())
        .collect();
    if let Some(recipient_index) = acme_pane_index_to_receive_removed_rows(&row_heights, pane_index)
    {
        row_heights[recipient_index] += removed_pane.geom.rows.as_usize();
    }

    let mut next_y = viewport.y;
    for (pane_geometry, rows) in column.pane_geometries.iter_mut().zip(row_heights) {
        pane_geometry.geom.y = next_y;
        pane_geometry.geom.rows = acme_rows_dimension(rows, viewport.rows);
        pane_geometry.geom.stacked = None;
        next_y += rows;
    }
    if next_y != viewport.y + viewport.rows {
        return Err(anyhow!("Acme column does not fill the viewport height"));
    }
    Ok(())
}

fn acme_pane_index_to_receive_removed_rows(
    row_heights: &[usize],
    removed_pane_index: usize,
) -> Option<usize> {
    if row_heights.is_empty() {
        return None;
    }
    let pane_above_removed = removed_pane_index.checked_sub(1);
    if row_heights
        .iter()
        .any(|&rows| rows > ACME_COLLAPSED_PANE_ROWS)
    {
        if let Some(index) = pane_above_removed.and_then(|index| {
            (0..=index)
                .rev()
                .find(|&index| row_heights[index] > ACME_COLLAPSED_PANE_ROWS)
        }) {
            return Some(index);
        }
        return (removed_pane_index..row_heights.len())
            .find(|&index| row_heights[index] > ACME_COLLAPSED_PANE_ROWS);
    }

    if removed_pane_index < row_heights.len() {
        Some(removed_pane_index)
    } else {
        pane_above_removed
    }
}

fn expand_acme_columns_after_removing_column(
    columns: &mut [AcmeColumn],
    removed_column_index: usize,
    removed_column_width: usize,
    viewport: Viewport,
) -> Result<()> {
    let recipient_index = if removed_column_index < columns.len() {
        removed_column_index
    } else {
        columns.len().saturating_sub(1)
    };
    let recipient_column = columns
        .get_mut(recipient_index)
        .ok_or_else(|| anyhow!("No Acme column can receive removed column width"))?;
    recipient_column.cols += removed_column_width;

    let mut next_x = viewport.x;
    for column in columns {
        column.x = next_x;
        next_x += column.cols;
    }
    if next_x != viewport.x + viewport.cols {
        return Err(anyhow!("Acme columns do not fill the viewport width"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acme_title_boundary_segments_include_column_boundary_edges() {
        let columns = vec![AcmeColumn {
            x: 10,
            cols: 20,
            pane_geometries: vec![
                AcmePaneGeometry {
                    pane_id: PaneId::Terminal(1),
                    geom: PaneGeom {
                        x: 10,
                        y: 0,
                        cols: Dimension::fixed(20),
                        rows: Dimension::fixed(5),
                        ..Default::default()
                    },
                },
                AcmePaneGeometry {
                    pane_id: PaneId::Terminal(2),
                    geom: PaneGeom {
                        x: 10,
                        y: 5,
                        cols: Dimension::fixed(20),
                        rows: Dimension::fixed(5),
                        ..Default::default()
                    },
                },
            ],
        }];

        assert_eq!(
            acme_own_line_title_boundary_segments(&columns),
            vec![(9, 4, 21)]
        );
    }
}
