<?php
/**
 * Individual scan rules. Each rule takes a product and returns false when
 * the product passes, or an issue descriptor array when it fails.
 */

function rule_missing_gtin( $product ) {
    if ( ! empty( $product->gtin ) ) {
        return false;
    }
    return array(
        'rule'    => 'missing_gtin',
        'level'   => 'warning',
        'message' => 'Product has no GTIN; Google may limit performance.',
    );
}

function rule_price_zero( $product ) {
    if ( $product->price > 0 ) {
        return false;
    }
    return array(
        'rule'    => 'price_zero',
        'level'   => 'critical',
        'message' => 'Product price is zero or missing.',
    );
}

function rule_image_unreachable( $product ) {
    $status = probe_image( $product->image_url );
    if ( 200 === $status ) {
        return false;
    }
    return array(
        'rule'    => 'image_unreachable',
        'level'   => 'critical',
        'message' => 'Primary image returned HTTP ' . $status . '.',
    );
}

function rule_description_short( $product ) {
    if ( strlen( $product->description ) >= 160 ) {
        return false;
    }
    return array(
        'rule'    => 'description_short',
        'level'   => 'warning',
        'message' => 'Description under 160 characters ranks poorly.',
    );
}

function probe_image( $url ) {
    $head = wp_remote_head( $url, array( 'timeout' => 5 ) );
    if ( is_wp_error( $head ) ) {
        return 0;
    }
    return (int) wp_remote_retrieve_response_code( $head );
}

function rule_title_length( $product ) {
    $len = mb_strlen( $product->title );
    if ( $len >= 20 && $len <= 150 ) {
        return false;
    }
    return array(
        'rule'    => 'title_length',
        'level'   => 'warning',
        'message' => 'Title length ' . $len . ' outside the 20-150 range Google prefers.',
    );
}

function rule_category_missing( $product ) {
    if ( ! empty( $product->google_category ) ) {
        return false;
    }
    return array(
        'rule'    => 'category_missing',
        'level'   => 'warning',
        'message' => 'No Google product category mapped.',
    );
}

function rule_availability_mismatch( $product ) {
    if ( $product->stock_status === $product->feed_availability ) {
        return false;
    }
    return array(
        'rule'    => 'availability_mismatch',
        'level'   => 'critical',
        'message' => 'Stock status disagrees with feed availability.',
    );
}

function rule_currency_consistent( $product ) {
    $shop_currency = get_woocommerce_currency();
    if ( $product->currency === $shop_currency ) {
        return false;
    }
    return array(
        'rule'    => 'currency_mismatch',
        'level'   => 'critical',
        'message' => 'Product currency ' . $product->currency . ' differs from shop currency ' . $shop_currency . '.',
    );
}

function rule_sale_price_sane( $product ) {
    if ( empty( $product->sale_price ) || $product->sale_price < $product->price ) {
        return false;
    }
    return array(
        'rule'    => 'sale_price_sane',
        'level'   => 'warning',
        'message' => 'Sale price is not lower than the regular price.',
    );
}

/**
 * The default rule set, in evaluation order. Cheap string checks run
 * before rules that hit the network or the database.
 *
 * @return array Callables, one per rule.
 */
function default_rules() {
    return array(
        'rule_price_zero',
        'rule_currency_consistent',
        'rule_availability_mismatch',
        'rule_missing_gtin',
        'rule_title_length',
        'rule_description_short',
        'rule_category_missing',
        'rule_sale_price_sane',
        'rule_image_unreachable',
    );
}
